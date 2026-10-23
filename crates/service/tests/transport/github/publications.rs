use super::*;
use std::path::Path;

pub(super) async fn exercise(
    f: &mut Fixture,
    server: &mut Process,
    owner: &Path,
    publisher: &Path,
    projection: Uuid,
    native: treeharbor_core::ObjectId,
    restored: &Path,
) {
    let publication = Uuid::new_v4();
    let id = publication.to_string();
    f.credential_path = owner.to_owned();
    let create = json!({"action":"publication_request","key":"pub-create","epoch":1,"publication":id,"specification":{"projection":projection.to_string(),"expected_head":null,"ttl_seconds":600}});
    let requested = result(request(f, &create));
    assert_eq!(requested["state"], "requested");
    assert_eq!(result(request(f, &create)), requested);
    let git = String::from_utf8(git_reference::git(restored, &["rev-parse", "main"], b""))
        .unwrap()
        .trim()
        .to_owned();
    let mapping = json!({"branch_id":requested["branch_id"],"native_revision":format!("native:{native}"),"git_revision":format!("git:sha1:{git}")});
    let prepare = json!({"action":"publication_prepare","key":"pub-map","epoch":1,"publication":id,"mapping":mapping});
    assert!(!request(f, &prepare).status.success());
    f.credential_path = publisher.to_owned();
    result(request(f, &prepare));
    let attempt = Uuid::new_v4();
    let begin = json!({"action":"publication_begin","key":"pub-begin","epoch":1,"publication":id,"attempt":attempt.to_string()});
    let body = f
        .credential()
        .request(json!({"method":"github","request":begin}));
    let wire = f.request_bytes(body.to_string().as_bytes(), "");
    // Kill before COMMIT: retry may issue the first dispatch. The trigger proves
    // the request actually reached the receipt-write boundary.
    let lock = (Uuid::new_v4().as_u128() & 0x7fff_ffff_ffff_ffff) as i64;
    f.sql
        .query_one("SELECT pg_advisory_lock($1)", &[&lock])
        .await
        .unwrap();
    f.sql.batch_execute(&format!("CREATE FUNCTION hold_pub() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='github.publication.begin.v1' THEN PERFORM pg_advisory_xact_lock({lock}::bigint); END IF; RETURN NEW; END $$; CREATE TRIGGER hold_pub AFTER INSERT ON th_operations FOR EACH ROW EXECUTE FUNCTION hold_pub();")).await.unwrap();
    let mut pending = UnixStream::connect(&f.config.socket).await.unwrap();
    pending.write_all(&wire).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if f.sql.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid::bigint=$1 AND objid::bigint=$2)",&[&(lock>>32),&(lock & 0xffff_ffff)]).await.unwrap().get::<_,bool>(0){break;}
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(pending);
    f.sql
        .query_one("SELECT pg_advisory_unlock($1)", &[&lock])
        .await
        .unwrap();
    f.sql
        .batch_execute("DROP TRIGGER hold_pub ON th_operations; DROP FUNCTION hold_pub()")
        .await
        .unwrap();
    assert_eq!(
        f.sql
            .query_one("SELECT state FROM th_github_publications", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "prepared"
    );
    *server = f.start().await;
    // Lose the next response after COMMIT. Replaying it must return the receipt
    // without ever issuing another dispatch flag.
    let mut pending = UnixStream::connect(&f.config.socket).await.unwrap();
    pending.write_all(&wire).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if f.sql
            .query_one(
                "SELECT count(*) FROM th_operations WHERE key='pub-begin'",
                &[],
            )
            .await
            .unwrap()
            .get::<_, i64>(0)
            == 1
        {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(pending);
    *server = f.start().await;
    let replay = result(request(f, &begin));
    assert_eq!(replay["dispatch_granted"], false);
    assert_eq!(replay["receipt"]["state"], "attempting");
    let mut another = begin.clone();
    another["key"] = json!("pub-begin-again");
    let credential = f.credential();
    let response = client::send(
        &f.config.socket,
        &credential.token,
        credential
            .request(json!({"method":"github","request":another}))
            .to_string()
            .into_bytes(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(response.status.as_u16(), 409);
    assert_eq!(response.body["error"]["code"], "host_publication_state");
    f.credential_path = owner.to_owned();
    let cancelled = result(request(
        f,
        &json!({"action":"publication_cancel","key":"pub-cancel","epoch":1,"publication":id}),
    ));
    assert_eq!(cancelled["cancel_requested"], true);
    assert_eq!(cancelled["state"], "attempting");
    f.credential_path = publisher.to_owned();
    let now: i64 = f
        .sql
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let mut observation = json!({"action":"publication_report","key":"pub-unknown","epoch":1,"publication":id,"report":{"attempt":attempt.to_string(),"branch_id":requested["branch_id"],"binding":requested["binding"],"native_revision":mapping["native_revision"],"prepared_git":mapping["git_revision"],"remote_revision":null,"outcome":"unknown","observed_at_unix_ms":now,"token_revoked":false}});
    assert_eq!(result(request(f, &observation))["state"], "uncertain");
    // Trusted observation fixture only: Git conversion above is real, but this
    // report does not claim a GitHub App or a remote branch was exercised here.
    observation["key"] = json!("pub-observed");
    observation["report"]["outcome"] = json!("observed");
    observation["report"]["remote_revision"] = mapping["git_revision"].clone();
    let observed = result(request(f, &observation));
    assert_eq!(observed["state"], "observed");
    assert_eq!(observed["merge_authorized"], false);
    assert_eq!(observed["cancel_requested"], true);
    assert_eq!(result(request(f, &observation)), observed);
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_outbox WHERE kind='github.publication.attempting'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    f.credential_path = owner.to_owned();
    let status = result(request(
        f,
        &json!({"action":"publication_status","epoch":1,"publication":id}),
    ));
    assert_eq!(status["state"], "observed");
}
