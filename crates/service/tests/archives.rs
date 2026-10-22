#![cfg(unix)]
use std::{fs, io::Cursor, os::unix::fs::PermissionsExt};
use treeharbor_core::ObjectId;
use treeharbor_service::{archive, pack::Pack};
fn fixture() -> (tempfile::TempDir, Vec<u8>, ObjectId) {
    let d = tempfile::tempdir().unwrap();
    fs::set_permissions(d.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let hex = include_str!("fixtures/source-archive-v1.hex").trim();
    let wire = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    let root = "sha256:e71cc0a85b8c6e91828159a8b0ed009933c210e88b382980121abad8d3410d37"
        .parse()
        .unwrap();
    (d, wire, root)
}
#[test]
fn independent_archive_produces_exact_full_pack_and_cannot_overwrite_it() {
    let (d, wire, root) = fixture();
    let out = d.path().join("pack");
    let p = archive::receive(&mut Cursor::new(&wire), root, &out).unwrap();
    assert_eq!(p.commit(), root);
    assert_eq!(p.object_count(), 5);
    assert_eq!(p.encoded_bytes(), 525);
    let raw = fs::read(out.join("manifest.json")).unwrap();
    assert_eq!(
        raw,
        include_bytes!("fixtures/source-pack-v3.json")
            .strip_suffix(b"\n")
            .unwrap()
    );
    assert!(archive::receive(&mut Cursor::new(&wire), root, &out).is_err());
    assert_eq!(Pack::open(&out).unwrap().commit(), root);
    let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    value["source_revision"] =
        serde_json::json!("git:sha1:0000000000000000000000000000000000000000");
    fs::write(out.join("manifest.json"), value.to_string()).unwrap();
    assert!(Pack::open(&out).is_err());
}
#[test]
fn truncation_corruption_wrong_root_trailing_and_oversized_inputs_never_publish() {
    let (d, wire, root) = fixture();
    let mut variants = vec![];
    for cut in [
        0,
        archive::MAGIC.len() - 1,
        archive::HEADER - 1,
        archive::HEADER,
        archive::HEADER + 39,
        wire.len() - 1,
    ] {
        variants.push(wire[..cut].to_vec());
    }
    let mut v = wire.clone();
    v.push(0);
    variants.push(v);
    let mut v = wire.clone();
    v[archive::HEADER + 40] ^= 1;
    variants.push(v);
    let mut v = wire.clone();
    v[archive::MAGIC.len()] ^= 1;
    variants.push(v);
    let mut v = wire.clone();
    v[archive::MAGIC.len() + 64..archive::MAGIC.len() + 68]
        .copy_from_slice(&100001u32.to_be_bytes());
    variants.push(v);
    let mut v = wire.clone();
    v[archive::HEADER + 32..archive::HEADER + 40].copy_from_slice(&u64::MAX.to_be_bytes());
    variants.push(v);
    for (i, v) in variants.iter().enumerate() {
        let out = d.path().join(format!("invalid-{i}"));
        assert!(
            archive::receive(&mut Cursor::new(v), root, &out).is_err(),
            "case {i}"
        );
        assert!(!out.exists());
    }
    assert_eq!(fs::read_dir(d.path()).unwrap().count(), 0);
}
#[test]
fn concurrent_download_completion_has_one_winner() {
    let (d, wire, root) = fixture();
    let out = d.path().join("pack");
    let results = std::thread::scope(|s| {
        let a = s.spawn(|| archive::receive(&mut Cursor::new(&wire), root, &out));
        let b = s.spawn(|| archive::receive(&mut Cursor::new(&wire), root, &out));
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(Pack::open(&out).unwrap().commit(), root);
}

#[test]
fn valid_frames_must_form_exact_typed_root_closure() {
    use treeharbor_core::Object;
    let (d, wire, root) = fixture();
    let mut frames = Vec::new();
    let mut offset = archive::HEADER;
    while offset < wire.len() {
        let len = u64::from_be_bytes(wire[offset + 32..offset + 40].try_into().unwrap());
        let end = offset + 40 + len as usize;
        frames.push(wire[offset..end].to_vec());
        offset = end;
    }
    let assemble = |records: &[Vec<u8>], expected: ObjectId| {
        let mut output = wire[..archive::HEADER].to_vec();
        output[archive::MAGIC.len()..archive::MAGIC.len() + 32]
            .copy_from_slice(expected.as_bytes());
        let count = archive::MAGIC.len() + 64;
        output[count..count + 4].copy_from_slice(&(records.len() as u32).to_be_bytes());
        let total: u64 = records.iter().map(|r| (r.len() - 40) as u64).sum();
        output[count + 4..count + 12].copy_from_slice(&total.to_be_bytes());
        for record in records {
            output.extend_from_slice(record);
        }
        output
    };
    let record = |object: Object| {
        let frame = object.encode().unwrap();
        let id = object.id().unwrap();
        let mut out = id.as_bytes().to_vec();
        out.extend_from_slice(&(frame.len() as u64).to_be_bytes());
        out.extend_from_slice(&frame);
        (id, out)
    };
    let mut variants = vec![(assemble(&frames[..frames.len() - 1], root), root)];
    let mut repeated = frames.clone();
    repeated.push(frames[0].clone());
    variants.push((assemble(&repeated, root), root));
    let mut unrelated = frames.clone();
    unrelated.push(record(Object::Blob(b"unreachable".to_vec())).1);
    variants.push((assemble(&unrelated, root), root));
    // Rehash the changed commit and pin that new root: all digests are valid,
    // but its snapshot edge points to a Blob, which must still be rejected.
    let Object::NativeCommit(mut commit) = Object::decode_verified(root, &frames[0][40..]).unwrap()
    else {
        panic!()
    };
    commit.snapshot = ObjectId::from_bytes(frames[4][..32].try_into().unwrap());
    let (new_root, new_record) = record(Object::NativeCommit(commit));
    let mut mistyped = frames.clone();
    mistyped[0] = new_record;
    variants.push((assemble(&mistyped, new_root), new_root));
    let mut wrong_snapshot = wire.clone();
    wrong_snapshot[archive::MAGIC.len() + 32] ^= 1;
    variants.push((wrong_snapshot, root));
    for (i, (archive, expected)) in variants.iter().enumerate() {
        let out = d.path().join(format!("invalid-graph-{i}"));
        assert!(archive::receive(&mut Cursor::new(archive), *expected, &out).is_err());
        assert!(!out.exists());
    }
    assert_eq!(fs::read_dir(d.path()).unwrap().count(), 0);
}

#[tokio::test]
#[ignore = "requires local Unix sockets; client fault transport in integration CI"]
async fn interrupted_or_mismatched_http_download_never_publishes_and_exact_retry_succeeds() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixListener,
    };
    use treeharbor_metadata::{Authority, Scope, access::SecretToken, exports::ExportFence};
    let (d, wire, root) = fixture();
    let socket = d.path().join("fault.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    // Deliberately fake transport for client-corruption testing; not evidence of
    // server authorization, which has separate actual PostgreSQL/HTTP tests.
    let credential = treeharbor_service::client::Credential {
        scope: Scope::new("test", uuid::Uuid::new_v4()).unwrap(),
        token: SecretToken::parse(&format!("thv1_{}", "0".repeat(64))).unwrap(),
    };
    let driver = tokio::spawn(async move {
        for case in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buffer[..n]);
                assert!(request.len() < 8192);
                if let Some(at) = request.windows(4).position(|p| p == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&request[..at])
                        .unwrap()
                        .to_ascii_lowercase();
                    let len: usize = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= at + 4 + len {
                        break;
                    }
                }
            }
            let mut response = wire.clone();
            if case == 1 {
                response[archive::MAGIC.len()] ^= 1;
            }
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                archive::CONTENT_TYPE,
                response.len()
            );
            stream.write_all(header.as_bytes()).await.unwrap();
            if case == 0 {
                response.pop();
            }
            stream.write_all(&response).await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    let out = d.path().join("result");
    let fence = ExportFence {
        authority: Authority::Standalone,
        epoch: 1,
        revision: root,
    };
    for _ in 0..2 {
        assert!(
            archive::download(
                &socket,
                &credential,
                fence,
                &out,
                std::time::Duration::from_secs(5)
            )
            .await
            .is_err()
        );
        assert!(!out.exists());
    }
    let complete = archive::download(
        &socket,
        &credential,
        fence,
        &out,
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert!(complete.status.is_success());
    assert_eq!(Pack::open(&out).unwrap().commit(), root);
    driver.await.unwrap();
}
