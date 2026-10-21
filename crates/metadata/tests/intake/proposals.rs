use super::*;
use treeharbor_metadata::{
    access::{Authorization, IssuedToken, Permission as P, PrincipalKind as K},
    proposals::{ChangeUploadSpec, ProposalCommand, ProposalStart},
    uploads::UploadLimits,
};

async fn owner(f: &mut Fixture, name: &str) -> (IssuedToken, Authorization) {
    f.db.provision_principal(f.scope.tenant(), name, K::Agent, "operator")
        .await
        .unwrap();
    f.db.set_repository_access(&f.scope, name, &[P::ChangeWrite], "operator")
        .await
        .unwrap();
    let token =
        f.db.issue_repository_token(&f.scope, name, &[P::ChangeWrite], 3600, "operator")
            .await
            .unwrap();
    let auth =
        f.db.authorize(&token.secret, &f.scope, P::ChangeWrite)
            .await
            .unwrap();
    (token, auth)
}
fn spec(run: Uuid) -> ChangeUploadSpec {
    ChangeUploadSpec {
        upload: Uuid::new_v4(),
        run,
        generation: 1,
        limits: UploadLimits {
            objects: 3,
            bytes: 1024,
            ttl_seconds: 60,
        },
    }
}
fn command(upload: Uuid, operations: Changes) -> ProposalCommand {
    ProposalCommand {
        key: format!("submit-{upload}"),
        authority: Authority::Standalone,
        epoch: 1,
        upload,
        changeset: Uuid::new_v4(),
        previous_version: 0,
        operations,
    }
}
async fn start(f: &mut Fixture, auth: &Authorization, spec: &ChangeUploadSpec) -> Value {
    f.db.begin_change_upload(
        &auth.context(format!("begin-{}", spec.upload), Authority::Standalone, 1),
        "local-1",
        spec,
    )
    .await
    .unwrap()
}
async fn upload(f: &mut Fixture, auth: &Authorization, session: Uuid, blob: &Object) {
    let object = PreparedObject::new(blob).unwrap();
    let ticket =
        f.db.reserve_change_blob(auth, session, &f.depot, &object)
            .await
            .unwrap();
    let receipt = f.depot.put(ticket.generation(), &object).unwrap();
    f.db.publish_change_blob(auth, &ticket, &receipt)
        .await
        .unwrap();
}
async fn finish(f: &mut Fixture, auth: &Authorization, cmd: &ProposalCommand) -> Value {
    let applied = prepare_change(&f.stage, f.base, &cmd.operations).unwrap();
    let ops = upload_graph(&mut f.db, &f.depot, &f.stage, applied.operations()).await;
    let result = upload_graph(&mut f.db, &f.depot, &f.stage, applied.result_snapshot()).await;
    f.db.finish_change_proposal(auth, cmd, &applied, &ops, &result)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn derived_objects_recover_the_same_reservation_and_share_the_session_budget() {
    let mut f = Fixture::new().await;
    let (_, auth) = owner(&mut f, "agent-owner").await;
    let mut s = spec(f.run);
    s.limits.objects = 2;
    let begin = start(&mut f, &auth, &s).await;
    let blob = Object::Blob(b"new".to_vec());
    upload(&mut f, &auth, s.upload, &blob).await;
    let dir = Object::Directory(
        Directory::new(vec![Entry {
            name: Name::new(b"file".to_vec()).unwrap(),
            value: EntryRef {
                mode: EntryMode::File,
                target: blob.id().unwrap(),
            },
        }])
        .unwrap(),
    );
    let object = PreparedObject::new(&dir).unwrap();
    let lease =
        f.db.reserve_proposal_object(&auth, s.upload, &f.depot, &object)
            .await
            .unwrap();
    let receipt = f.depot.put(lease.key().generation(), &object).unwrap();
    f.db = Metadata::new(connection(&f.schema).await).await.unwrap();
    let again =
        f.db.reserve_proposal_object(&auth, s.upload, &f.depot, &object)
            .await
            .unwrap();
    assert_eq!(again.token(), lease.token());
    assert_eq!(again.key(), lease.key());
    f.db.publish_object(&again, &receipt).await.unwrap();
    assert!(
        f.db.try_retire_object(again.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    let raw = connection(&f.schema).await;
    let deadline:i64=raw.query_one("SELECT (extract(epoch from expires_at)*1000)::bigint FROM th_object_holds WHERE token=$1",&[&again.token()]).await.unwrap().get(0);
    assert_eq!(json!(deadline), begin["expires_at_unix_ms"]);
    let status = f.db.change_upload_status(&auth, s.upload).await.unwrap();
    assert_eq!(status["reserved_objects"], 2);
    let snapshot = PreparedObject::new(&Object::Snapshot { root: object.id() }).unwrap();
    assert!(matches!(
        f.db.reserve_proposal_object(&auth, s.upload, &f.depot, &snapshot)
            .await,
        Err(MetaError::UploadLimit)
    ));
    // Terminal legacy records have no managed runtime-removal evidence.
    raw.execute(
        "UPDATE th_agent_runs SET state='failed' WHERE run=$1",
        &[&f.run],
    )
    .await
    .unwrap();
    let retry = spec(f.run);
    assert!(matches!(
        f.db.begin_change_upload(
            &auth.context("legacy-terminal".into(), Authority::Standalone, 1),
            "local-1",
            &retry
        )
        .await,
        Err(MetaError::ExecutionState)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn session_expiry_inside_acceptance_rolls_back_version_pins_and_receipt() {
    let mut f = Fixture::new().await;
    let (_, auth) = owner(&mut f, "agent-owner").await;
    let mut s = spec(f.run);
    s.limits.ttl_seconds = 1;
    let operations = changes(&f.stage, b"expire");
    let applied = prepare_change(&f.stage, f.base, &operations).unwrap();
    let ops = upload_graph(&mut f.db, &f.depot, &f.stage, applied.operations()).await;
    let result = upload_graph(&mut f.db, &f.depot, &f.stage, applied.result_snapshot()).await;
    start(&mut f, &auth, &s).await;
    let blob = f
        .stage
        .get(operations.operations()[0].replacement.unwrap().target)
        .unwrap();
    upload(&mut f, &auth, s.upload, &blob).await;
    let raw = connection(&f.schema).await;
    raw.batch_execute("CREATE FUNCTION delay_proposal() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(1.1); RETURN NEW; END $$; CREATE TRIGGER delay_proposal BEFORE INSERT ON th_change_versions FOR EACH ROW EXECUTE FUNCTION delay_proposal()").await.unwrap();
    let cmd = command(s.upload, operations);
    assert!(matches!(
        f.db.finish_change_proposal(&auth, &cmd, &applied, &ops, &result)
            .await,
        Err(MetaError::UploadState)
    ));
    let row=raw.query_one("SELECT (SELECT count(*) FROM th_change_versions),(SELECT count(*) FROM th_changesets),(SELECT count(*) FROM th_object_pins WHERE holder LIKE 'change:%'),(SELECT count(*) FROM th_operations WHERE kind='change.submit.v1')",&[]).await.unwrap();
    for i in 0..4 {
        assert_eq!(row.get::<_, i64>(i), 0);
    }
    assert_eq!(
        f.db.change_upload_status(&auth, s.upload).await.unwrap()["state"],
        "active"
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn proposal_resumes_after_lost_blob_response_and_accepts_exact_version_once() {
    let mut f = Fixture::new().await;
    let (token, auth) = owner(&mut f, "agent-owner").await;
    let s = spec(f.run);
    let begin = start(&mut f, &auth, &s).await;
    assert_eq!(begin, start(&mut f, &auth, &s).await);
    let operations = changes(&f.stage, b"replacement from agent");
    let blob = f
        .stage
        .get(operations.operations()[0].replacement.unwrap().target)
        .unwrap();
    let object = PreparedObject::new(&blob).unwrap();
    let ticket =
        f.db.reserve_change_blob(&auth, s.upload, &f.depot, &object)
            .await
            .unwrap();
    let receipt = f.depot.put(ticket.generation(), &object).unwrap();
    // Byte write completed, but its publication response was lost with the connection.
    f.db = Metadata::new(connection(&f.schema).await).await.unwrap();
    let again =
        f.db.reserve_change_blob(&auth, s.upload, &f.depot, &object)
            .await
            .unwrap();
    assert_eq!(ticket.generation(), again.generation());
    f.db.publish_change_blob(&auth, &again, &receipt)
        .await
        .unwrap();
    upload(&mut f, &auth, s.upload, &blob).await;
    let status = f.db.change_upload_status(&auth, s.upload).await.unwrap();
    assert_eq!(status["reserved_objects"], 1);
    assert_eq!(status["expires_at_unix_ms"], begin["expires_at_unix_ms"]);
    let cmd = command(s.upload, operations);
    let ProposalStart::Ready(read) = f.db.open_change_proposal(&auth, &cmd).await.unwrap() else {
        panic!("not yet committed")
    };
    assert_eq!(read.snapshot(), f.base);
    let root =
        f.db.open_proposal_source_batch(&read, &f.depot, &[read.root()], 1024 * 1024)
            .await
            .unwrap()
            .read()
            .unwrap();
    assert!(matches!(root[0].object(), Object::NativeCommit(_)));
    let replacement = cmd.operations.operations()[0].replacement.unwrap().target;
    let bytes =
        f.db.open_proposal_blob(&read, &f.depot, replacement)
            .await
            .unwrap()
            .read()
            .unwrap();
    assert_eq!(bytes[0].object(), &blob);
    let applied = prepare_change(&f.stage, f.base, &cmd.operations).unwrap();
    let ops = upload_graph(&mut f.db, &f.depot, &f.stage, applied.operations()).await;
    let result = upload_graph(&mut f.db, &f.depot, &f.stage, applied.result_snapshot()).await;
    let mut second = Metadata::new(connection(&f.schema).await).await.unwrap();
    let (a, b) = tokio::join!(
        f.db.finish_change_proposal(&auth, &cmd, &applied, &ops, &result),
        second.finish_change_proposal(&auth, &cmd, &applied, &ops, &result)
    );
    let accepted = a.unwrap();
    assert_eq!(accepted, b.unwrap());
    assert_eq!(accepted["state"], "accepted");
    assert_eq!(
        f.db.change_upload_status(&auth, s.upload).await.unwrap()["state"],
        "completed"
    );
    f.db.close_change_proposal(*read).await.unwrap();
    f.db.drain_workspace(&context(&f.scope, "drain-after-accept"), f.workspace, 1)
        .await
        .unwrap();
    let ProposalStart::Replay(replay) = f.db.open_change_proposal(&auth, &cmd).await.unwrap()
    else {
        panic!("receipt must precede stale workspace check")
    };
    assert_eq!(replay, accepted);
    assert_eq!(
        f.db.change_version(&f.scope, cmd.changeset, 1)
            .await
            .unwrap(),
        accepted
    );
    let raw = connection(&f.schema).await;
    let counts=raw.query_one("SELECT (SELECT count(*) FROM th_change_versions),(SELECT count(*) FROM th_outbox WHERE kind='change.accepted')",&[]).await.unwrap();
    assert_eq!((counts.get::<_, i64>(0), counts.get::<_, i64>(1)), (1, 1));
    f.db.revoke_repository_token(&f.scope, token.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.open_change_proposal(&auth, &cmd).await,
        Err(MetaError::Unauthenticated)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn proposal_cannot_borrow_guessed_blobs_other_uploads_owners_or_source_authority() {
    let mut f = Fixture::new().await;
    let (token, auth) = owner(&mut f, "agent-owner").await;
    let (_, other) = owner(&mut f, "other-owner").await;
    let s = spec(f.run);
    start(&mut f, &auth, &s).await;
    assert!(matches!(
        f.db.authorize(&token.secret, &f.scope, P::RepositoryWrite)
            .await,
        Err(MetaError::Forbidden)
    ));
    assert!(matches!(
        f.db.source_upload_status(&auth, s.upload).await,
        Err(MetaError::Forbidden)
    ));
    assert!(matches!(
        f.db.change_upload_status(&other, s.upload).await,
        Err(MetaError::NotFound)
    ));
    let bad = PreparedObject::new(&Object::Directory(Directory::new(vec![]).unwrap())).unwrap();
    assert!(matches!(
        f.db.reserve_change_blob(&auth, s.upload, &f.depot, &bad)
            .await,
        Err(MetaError::Invalid(_))
    ));
    let operations = changes(&f.stage, b"known CAS bytes");
    let blob = f
        .stage
        .get(operations.operations()[0].replacement.unwrap().target)
        .unwrap();
    // Trusted import into the same tenant catalog is not membership in this session.
    upload_graph(&mut f.db, &f.depot, &f.stage, blob.id().unwrap()).await;
    let cmd = command(s.upload, operations);
    assert!(matches!(
        f.db.open_change_proposal(&auth, &cmd).await,
        Err(MetaError::UploadMembership)
    ));
    let s2 = spec(f.run);
    start(&mut f, &auth, &s2).await;
    upload(&mut f, &auth, s2.upload, &blob).await;
    assert!(matches!(
        f.db.open_change_proposal(&auth, &cmd).await,
        Err(MetaError::UploadMembership)
    ));
    upload(&mut f, &auth, s.upload, &blob).await;
    let ProposalStart::Ready(read) = f.db.open_change_proposal(&auth, &cmd).await.unwrap() else {
        panic!()
    };
    let foreign = put(&f.stage, Object::Blob(b"foreign secret".to_vec()));
    upload_graph(&mut f.db, &f.depot, &f.stage, foreign).await;
    assert!(matches!(
        f.db.open_proposal_blob(&read, &f.depot, foreign).await,
        Err(MetaError::UploadMembership)
    ));
    f.db.close_change_proposal(*read).await.unwrap();
    let accepted = finish(&mut f, &auth, &cmd).await;
    assert_eq!(accepted["version"], 1);
    let mut different = cmd;
    different.previous_version = 1;
    assert!(matches!(
        f.db.open_change_proposal(&auth, &different).await,
        Err(MetaError::IdempotencyConflict)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn proposal_budget_deadline_drain_and_version_collision_do_not_partially_commit() {
    let mut f = Fixture::new().await;
    let (_, auth) = owner(&mut f, "agent-owner").await;
    let mut s = spec(f.run);
    s.limits.objects = 1;
    s.limits.bytes = 64;
    start(&mut f, &auth, &s).await;
    let blob = Object::Blob(b"one".to_vec());
    upload(&mut f, &auth, s.upload, &blob).await;
    let extra = PreparedObject::new(&Object::Blob(b"two".to_vec())).unwrap();
    assert!(matches!(
        f.db.reserve_change_blob(&auth, s.upload, &f.depot, &extra)
            .await,
        Err(MetaError::UploadLimit)
    ));
    let cmd = command(s.upload, changes(&f.stage, b"one"));
    let accepted = finish(&mut f, &auth, &cmd).await;
    let s2 = spec(f.run);
    start(&mut f, &auth, &s2).await;
    upload(&mut f, &auth, s2.upload, &blob).await;
    let mut collision = command(s2.upload, changes(&f.stage, b"one"));
    collision.changeset = cmd.changeset;
    let applied = prepare_change(&f.stage, f.base, &collision.operations).unwrap();
    let ops = upload_graph(&mut f.db, &f.depot, &f.stage, applied.operations()).await;
    let result = upload_graph(&mut f.db, &f.depot, &f.stage, applied.result_snapshot()).await;
    assert!(matches!(
        f.db.finish_change_proposal(&auth, &collision, &applied, &ops, &result)
            .await,
        Err(MetaError::VersionConflict)
    ));
    assert_eq!(
        f.db.change_upload_status(&auth, s2.upload).await.unwrap()["state"],
        "active"
    );
    assert_eq!(
        f.db.change_version(&f.scope, cmd.changeset, 1)
            .await
            .unwrap(),
        accepted
    );
    let mut expires = spec(f.run);
    expires.limits.ttl_seconds = 1;
    start(&mut f, &auth, &expires).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(matches!(
        f.db.reserve_change_blob(&auth, expires.upload, &f.depot, &extra)
            .await,
        Err(MetaError::UploadState)
    ));
    f.db.drain_workspace(&context(&f.scope, "drain-before-accept"), f.workspace, 1)
        .await
        .unwrap();
    assert!(matches!(
        f.db.finish_change_proposal(&auth, &collision, &applied, &ops, &result)
            .await,
        Err(MetaError::ExecutionState)
    ));
    assert!(matches!(
        f.db.reserve_change_blob(&auth, s2.upload, &f.depot, &extra)
            .await,
        Err(MetaError::ExecutionState)
    ));
    let abort = auth.context("abort-after-drain".into(), Authority::Standalone, 1);
    f.db.abort_change_upload(&abort, s2.upload).await.unwrap();
    assert_eq!(
        f.db.change_upload_status(&auth, s2.upload).await.unwrap()["state"],
        "aborted"
    );
    let raw = connection(&f.schema).await;
    assert!(
        raw.execute(
            "UPDATE th_change_uploads SET run=$1 WHERE upload=$2",
            &[&Uuid::new_v4(), &s2.upload]
        )
        .await
        .is_err()
    );
    cleanup(&f.schema).await;
}
