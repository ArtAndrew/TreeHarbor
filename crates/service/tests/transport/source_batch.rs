use super::*;

async fn begin(f: &Fixture, upload: Uuid, count: usize) {
    let reply = f
        .send(json!({
            "method": "source_begin", "key": format!("begin-{upload}"),
            "authority": "standalone", "epoch": 1, "upload": upload,
            "ttl_seconds": 120, "max_objects": count, "max_bytes": 65536
        }))
        .await;
    assert!(reply.status.is_success(), "{}", reply.body);
}

async fn put(f: &Fixture, upload: Uuid, credential: &Credential, body: &[u8]) -> (u16, Value) {
    let mut raw = format!(
        "PUT /v1/source/{}/{}/uploads/{}/blobs HTTP/1.1\r\nHost: treeharbor.local\r\nContent-Type: application/octet-stream\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        f.scope.tenant(),
        f.scope.repository(),
        upload,
        credential.token.expose(),
        body.len()
    ).into_bytes();
    raw.extend_from_slice(body);
    let response = f.raw(&raw).await;
    let header = response
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap();
    let status = std::str::from_utf8(&response[9..12])
        .unwrap()
        .parse()
        .unwrap();
    let body = serde_json::from_slice(&response[header + 4..]).unwrap();
    (status, body)
}

async fn counts(f: &Fixture, upload: Uuid) -> (i64, i64) {
    let row = f.sql.query_one(
        "SELECT reserved_objects,(SELECT count(*) FROM th_source_upload_items i WHERE i.upload=u.upload AND i.complete) FROM th_source_uploads u WHERE u.upload=$1",
        &[&upload],
    ).await.unwrap();
    (row.get(0), row.get(1))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "disposable PostgreSQL and actual binary HTTP transport"]
async fn batch_budget_and_publication_are_atomic_and_replay_survives_restart() {
    let mut fixture = Fixture::new().await;
    let agent = fixture.importer();
    let credential = fixture.credential();
    let mut server = fixture.start().await;
    let mut frames = [
        Object::Blob(b"alpha".to_vec()),
        Object::Blob(b"beta".to_vec()),
    ]
    .map(|object| (object.id().unwrap(), object.encode().unwrap()));
    frames.sort_unstable_by_key(|frame| frame.0);
    let mut body = b"THSB\x01\x00\x02".to_vec();
    for (_, frame) in &frames {
        body.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        body.extend_from_slice(frame);
    }
    let limited = Uuid::new_v4();
    begin(&fixture, limited, 1).await;
    let (status, _) = put(&fixture, limited, &credential, &body).await;
    assert_eq!(status, 429);
    assert_eq!(counts(&fixture, limited).await, (0, 0));

    let upload = Uuid::new_v4();
    begin(&fixture, upload, 2).await;
    let (status, _) = put(&fixture, upload, &agent, &body).await;
    assert_eq!(status, 403);
    let (status, _) = put(&fixture, upload, &credential, &body[..body.len() - 1]).await;
    assert_eq!(status, 400);
    assert_eq!(counts(&fixture, upload).await, (0, 0));
    fixture
        .sql
        .batch_execute(
            "
        CREATE FUNCTION fail_batch_publication() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.complete THEN RAISE EXCEPTION 'fixture publication failure'; END IF;
            RETURN NEW;
        END
        $$;
        CREATE TRIGGER fail_batch_publication BEFORE UPDATE ON th_source_upload_items
        FOR EACH ROW EXECUTE FUNCTION fail_batch_publication();
    ",
        )
        .await
        .unwrap();
    let (status, _) = put(&fixture, upload, &credential, &body).await;
    assert_eq!(status, 500);
    assert_eq!(counts(&fixture, upload).await, (2, 0));
    fixture.sql.batch_execute(
        "DROP TRIGGER fail_batch_publication ON th_source_upload_items; DROP FUNCTION fail_batch_publication()"
    ).await.unwrap();

    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    let server = fixture.start().await;
    let expected = json!({"version":1,"result":{
        "objects": frames.map(|(id, _)| id.to_string()), "state":"stored"
    }});
    assert_eq!(
        put(&fixture, upload, &credential, &body).await,
        (200, expected.clone())
    );
    assert_eq!(
        put(&fixture, upload, &credential, &body).await,
        (200, expected)
    );
    assert_eq!(counts(&fixture, upload).await, (2, 2));
    drop(server);
    fixture.cleanup().await;
}
