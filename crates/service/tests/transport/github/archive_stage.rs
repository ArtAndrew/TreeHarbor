use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use treeharbor_core::ObjectId;
use treeharbor_git_bridge::{BundleInput, Limits, import_branches};
use treeharbor_github::{ArchivedBranches, InventoryHead, InventoryObject};
use treeharbor_metadata::github::ArchiveBranchTip;
use treeharbor_storage::LocalStore;

fn archive_spool(f: &Fixture, bind: &Value) -> (PathBuf, Value) {
    let dir = f.dir.path();
    let git = dir.join("archive.git");
    git_reference::git(
        dir,
        &[
            "init",
            "--bare",
            "--object-format=sha1",
            "--template=",
            git.to_str().unwrap(),
        ],
        b"",
    );
    let tree = git_reference::hash(&git, "tree", b"");
    let commit = |label: &str, parents: &[&str]| {
        let mut raw = format!("tree {tree}\n");
        for parent in parents {
            raw.push_str(&format!("parent {parent}\n"));
        }
        raw.push_str(&format!(
            "author Fixture <fixture@example.invalid> 1234567890 +0000\ncommitter Fixture <fixture@example.invalid> 1234567890 +0000\n\n{label}\n"
        ));
        git_reference::hash(&git, "commit", raw.as_bytes())
    };
    let first = commit("first parent", &[]);
    let second = commit("second parent", &[]);
    let main = commit("merge", &[&first, &second]);
    let topic = commit("independent branch", &[]);
    let mut refs = BTreeMap::new();
    let mut heads = Vec::new();
    for (name, oid) in [("main", &main), ("stable", &main), ("topic", &topic)] {
        let reference = format!("refs/heads/{name}");
        git_reference::git(&git, &["update-ref", &reference, oid], b"");
        let RevisionRef::Git(id) = format!("git:sha1:{oid}").parse().unwrap() else {
            panic!("expected Git tip");
        };
        refs.insert(reference, id);
        heads.push(InventoryHead {
            id: format!("fixture-ref-{name}"),
            name: name.into(),
            prefix: "refs/heads/".into(),
            target: InventoryObject {
                kind: "Commit".into(),
                oid: oid.clone(),
            },
        });
    }
    let bundle = dir.join("archive.bundle");
    git_reference::git(
        &git,
        &["bundle", "create", bundle.to_str().unwrap(), "--branches"],
        b"",
    );
    let spool = dir.join("archive-spool");
    fs::create_dir(&spool).unwrap();
    fs::set_permissions(&spool, fs::Permissions::from_mode(0o700)).unwrap();
    let store = LocalStore::open(&spool, &TenantId::new("archive-preflight").unwrap()).unwrap();
    let imported = import_branches(
        &store,
        BundleInput {
            git: Path::new("/usr/bin/git"),
            bundle: &bundle,
            scratch: &spool,
            limits: Limits::default(),
        },
        &refs,
    )
    .unwrap();
    let branches: BTreeMap<_, _> = imported
        .refs
        .into_iter()
        .map(|(reference, tip)| {
            (
                reference,
                ArchiveBranchTip {
                    git_revision: RevisionRef::Git(tip.origin).to_string(),
                    native_revision: RevisionRef::Native(tip.commit).to_string(),
                    snapshot: tip.snapshot.to_string(),
                },
            )
        })
        .collect();
    let objects: Vec<_> = imported.objects.iter().map(ToString::to_string).collect();
    let archive = ArchivedBranches {
        version: 1,
        binding: serde_json::from_value(bind["specification"]["upstream"].clone()).unwrap(),
        app_id: bind["specification"]["app_id"].as_u64().unwrap(),
        repository_node_id: "fixture-repository".into(),
        default_branch: "main".into(),
        started_at_unix: 1,
        finished_at_unix: 2,
        heads,
        token_revoked: true,
    };
    let bytes = format!(
        r#"{{"format":"treeharbor-archive-branch-spool","version":1,"archive":{},"branches":{},"objects":{},"encoded_bytes":{}}}"#,
        serde_json::to_string(&archive).unwrap(),
        serde_json::to_string(&branches).unwrap(),
        serde_json::to_string(&objects).unwrap(),
        imported.encoded_bytes,
    );
    files::write_private(&spool.join("manifest.json"), bytes.as_bytes()).unwrap();
    let mut hash = Sha256::new();
    hash.update(b"treeharbor-archive-branch-spool\0v1\0");
    hash.update(bytes.as_bytes());
    let fingerprint = ObjectId::from_bytes(hash.finalize().into()).to_string();
    fs::remove_dir_all(&git).unwrap();
    fs::remove_file(&bundle).unwrap();
    let spec = json!({
        "version": 1,
        "spool_fingerprint": fingerprint,
        "app_id": archive.app_id,
        "upstream": archive.binding,
        "default_branch": archive.default_branch,
        "branches": branches,
        "object_count": objects.len(),
        "encoded_bytes": imported.encoded_bytes,
    });
    (spool, spec)
}

fn assert_rejected(output: Output, code: &str) {
    assert!(!output.status.success());
    let body: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "expected {code}, received {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(body["error"]["code"], code, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL, real Git bundle, no live GitHub App"]
async fn archive_stage_retains_all_roots_atomically_with_one_resumable_upload() {
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    f.importer();
    let bridge = provision_bridge(&f);
    let bind: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/github-bind-v1.json"
    ))
    .unwrap();
    let binding = bind["binding"].as_str().unwrap();
    let (spool, spec) = archive_spool(&f, &bind);
    let journal = f.dir.path().join("archive-stage.json");
    let invoke = |binding: &str, state: &Path| {
        f.cli(&[
            "archive-stage",
            "--spool",
            spool.to_str().unwrap(),
            "--archive-fingerprint",
            spec["spool_fingerprint"].as_str().unwrap(),
            "--binding",
            binding,
            "--epoch",
            "1",
            "--state",
            state.to_str().unwrap(),
        ])
    };
    let server = f.start().await;
    result(request(&f, &bind));
    let missing = Uuid::new_v4().to_string();
    assert_rejected(
        invoke(&missing, &f.dir.path().join("wrong-binding.json")),
        "host_binding_state",
    );
    let read_counts = || async {
        let row = f
            .sql
            .query_one(
                "SELECT (SELECT count(*) FROM th_bases),(SELECT count(*) FROM th_object_pins),
                (SELECT count(*) FROM th_source_uploads),(SELECT count(*) FROM th_native_refs)",
                &[],
            )
            .await
            .unwrap();
        std::array::from_fn::<_, 4, _>(|index| row.get::<_, i64>(index))
    };
    assert_eq!(read_counts().await, [0, 0, 0, 0]);
    // Fail after registering the roots and closing the upload, before commit.
    f.sql
        .batch_execute(
            r#"
        CREATE FUNCTION reject_archive_stage() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.kind='github.archive.stage.v1' THEN
                RAISE EXCEPTION 'injected failure before committing archive receipt';
            END IF;
            RETURN NEW;
        END;
        $$;
        CREATE TRIGGER reject_archive_stage BEFORE INSERT ON th_operations
          FOR EACH ROW EXECUTE FUNCTION reject_archive_stage();
        "#,
        )
        .await
        .unwrap();
    assert_rejected(invoke(binding, &journal), "internal_error");
    assert_eq!(read_counts().await, [0, 0, 1, 0]);
    let state: Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    assert_eq!(state["intent"]["specification"], spec);
    let upload: Uuid = state["upload"].as_str().unwrap().parse().unwrap();
    let uploaded = f
        .sql
        .query_one(
            "SELECT u.state,count(i.digest),COALESCE(sum(i.length),0)::bigint,bool_and(i.complete)
         FROM th_source_uploads u JOIN th_source_upload_items i USING (tenant,repository,upload)
         WHERE u.upload=$1 GROUP BY u.state",
            &[&upload],
        )
        .await
        .unwrap();
    assert_eq!(uploaded.get::<_, &str>(0), "active");
    assert_eq!(
        uploaded.get::<_, i64>(1),
        spec["object_count"].as_i64().unwrap()
    );
    assert_eq!(
        uploaded.get::<_, i64>(2),
        spec["encoded_bytes"].as_i64().unwrap()
    );
    assert!(uploaded.get::<_, bool>(3));
    f.sql.batch_execute("DROP TRIGGER reject_archive_stage ON th_operations; DROP FUNCTION reject_archive_stage()").await.unwrap();

    let mut stage = json!({
        "action":"archive_stage","key":"omitted-root","epoch":1,"binding":binding,
        "upload":upload,"specification":spec,
    });
    stage["specification"]["branches"]
        .as_object_mut()
        .unwrap()
        .remove("refs/heads/topic");
    assert_rejected(request(&f, &stage), "upload_membership");
    stage["key"] = json!("different-origin");
    stage["specification"] = spec.clone();
    stage["specification"]["branches"]["refs/heads/topic"]["git_revision"] =
        json!(format!("git:sha1:{}", "ab".repeat(20)));
    assert_rejected(request(&f, &stage), "upload_membership");
    assert_eq!(read_counts().await, [0, 0, 1, 0]);
    let bridge_request = f.dir.path().join("bridge-stage-request.json");
    files::write_private(&bridge_request, &serde_json::to_vec(&stage).unwrap()).unwrap();
    assert_rejected(
        f.cli_with_credential(
            &bridge,
            &["github", "--request", bridge_request.to_str().unwrap()],
        ),
        "forbidden",
    );
    drop(server);
    let server = f.start().await;
    let receipt = result(invoke(binding, &journal));
    assert_eq!(
        receipt,
        json!({"version":1,"tenant":f.scope.tenant(),"repository":f.scope.repository(),
            "authority":"connected","epoch":1,"binding":binding,"upload":upload,
            "state":"staged","specification":spec})
    );
    assert_eq!(read_counts().await, [2, 2, 1, 0]);
    let rows = f
        .sql
        .query(
            "SELECT b.revision,b.snapshot,p.digest FROM th_bases b JOIN th_object_pins p
         ON p.tenant=b.tenant AND p.repository=b.repository AND p.holder=b.pin_holder",
            &[],
        )
        .await
        .unwrap();
    let tips = spec["branches"].as_object().unwrap();
    for name in ["refs/heads/main", "refs/heads/topic"] {
        let tip = &tips[name];
        let revision = tip["native_revision"].as_str().unwrap();
        let RevisionRef::Native(commit) = revision.parse().unwrap() else {
            panic!("expected native tip");
        };
        let snapshot: ObjectId = tip["snapshot"].as_str().unwrap().parse().unwrap();
        let row = rows
            .iter()
            .find(|row| row.get::<_, &str>(0) == revision)
            .unwrap();
        assert_eq!(row.get::<_, Vec<u8>>(1), snapshot.as_bytes());
        assert_eq!(row.get::<_, Vec<u8>>(2), commit.as_bytes());
    }
    let repository = f
        .sql
        .query_one("SELECT authority,epoch,state FROM th_repositories", &[])
        .await
        .unwrap();
    assert_eq!(repository.get::<_, &str>(0), "connected");
    assert_eq!(repository.get::<_, i64>(1), 1);
    assert_eq!(repository.get::<_, &str>(2), "active");
    let ended = f
        .sql
        .query_one(
            "UPDATE th_source_uploads SET expires_at=clock_timestamp()-interval '1 second'
         WHERE upload=$1 RETURNING state",
            &[&upload],
        )
        .await
        .unwrap();
    assert_eq!(ended.get::<_, &str>(0), "completed");
    drop(server);
    let server = f.start().await;
    assert_eq!(result(invoke(binding, &journal)), receipt);
    stage["key"] = json!(format!("archive-{upload}-stage"));
    stage["specification"] = spec.clone();
    stage["specification"]["spool_fingerprint"] = json!(ObjectId::from_bytes([99; 32]).to_string());
    assert_rejected(request(&f, &stage), "idempotency_conflict");
    assert_eq!(read_counts().await, [2, 2, 1, 0]);
    drop(server);
    f.cleanup().await;
}
