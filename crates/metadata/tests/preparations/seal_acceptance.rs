//! Trusted capture fixture; actual owner/DB acceptance, not sandbox evidence.
use super::*;
use treeharbor_core::{AppliedChange, ObjectStore, prepare_change};
use treeharbor_metadata::{
    access::Authorization,
    catalog::ObjectLease,
    seals::{SealAcceptCommand, SealAcceptRead, SealAcceptStart},
};
use treeharbor_storage::LocalStore;
fn command(seal: Uuid) -> SealAcceptCommand {
    SealAcceptCommand {
        key: Uuid::new_v4().to_string(),
        authority: Authority::Standalone,
        epoch: 1,
        seal,
        changeset: Uuid::new_v4(),
        previous_version: 0,
        ttl_seconds: 300,
    }
}
async fn ready() -> (Fixture, Owner, Node, Uuid, Vec<Object>) {
    let (mut f, owner, node, input, plan) = setup_capture().await;
    let (items, deps) = objects(false);
    let store = depot(&f);
    f.db.begin_seal_publication(
        &node.auth,
        &plan,
        &store,
        &declaration(&items, deps, [91; 32]),
    )
    .await
    .unwrap();
    for o in &items {
        upload(&mut f, &node, &plan, o).await;
    }
    f.db.finish_seal_publication(&node.auth, &plan, [91; 32])
        .await
        .unwrap();
    (f, owner, node, input.seal, items)
}
async fn open(
    f: &mut Fixture,
    owner: &Owner,
    cmd: &SealAcceptCommand,
) -> (Authorization, Box<SealAcceptRead>) {
    let auth =
        f.db.authorize(&owner.token.secret, &f.scope, P::ChangeWrite)
            .await
            .unwrap();
    let SealAcceptStart::Ready(read) = f.db.open_seal_acceptance(&auth, cmd).await.unwrap() else {
        panic!()
    };
    (auth, read)
}
fn compute(f: &Fixture, items: &[Object]) -> (AppliedChange, Vec<PreparedObject>) {
    let temp = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = LocalStore::open(temp.path(), &TenantId::new("accept-compute").unwrap()).unwrap();
    let root = store
        .put(&Object::Directory(Directory::new(vec![]).unwrap()))
        .unwrap();
    assert_eq!(store.put(&Object::Snapshot { root }).unwrap(), f.snapshot);
    for o in items {
        store.put(o).unwrap();
    }
    let Object::ChangeOperations(changes) = items.last().unwrap() else {
        panic!()
    };
    let applied = prepare_change(&store, f.snapshot, changes).unwrap();
    let snapshot = store.get(applied.result_snapshot()).unwrap();
    let Object::Snapshot { root } = snapshot else {
        panic!()
    };
    let frames = [
        store.get(applied.operations()).unwrap(),
        store.get(root).unwrap(),
        snapshot,
    ]
    .iter()
    .map(|o| PreparedObject::new(o).unwrap())
    .collect();
    (applied, frames)
}
async fn publish(
    f: &mut Fixture,
    read: &SealAcceptRead,
    frames: &[PreparedObject],
) -> (ObjectLease, ObjectLease) {
    let store = depot(f);
    let mut ops = None;
    let mut result = None;
    for frame in frames {
        let lease =
            f.db.reserve_seal_accept_object(read, &store, frame)
                .await
                .unwrap();
        if lease.requires_upload() {
            f.db.publish_object(&lease, &store.put(lease.key().generation(), frame).unwrap())
                .await
                .unwrap();
        }
        if frame.kind() == 4 {
            ops = Some(lease);
        } else if frame.kind() == 3 {
            result = Some(lease);
        }
    }
    (ops.unwrap(), result.unwrap())
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; owner acceptance and retained lineage"]
async fn seal_acceptance_replays_after_drain_and_retains_original_run_base_and_pins() {
    let (mut f, owner, node, id, items) = ready().await;
    let cmd = command(id);
    assert!(f.db.open_seal_acceptance(&node.auth, &cmd).await.is_err());
    let (auth, read) = open(&mut f, &owner, &cmd).await;
    let (applied, frames) = compute(&f, &items);
    let (ops, result) = publish(&mut f, &read, &frames).await;
    let accepted =
        f.db.finish_seal_acceptance(&read, &applied, &ops, &result)
            .await
            .unwrap();
    assert_eq!(accepted["version"], 1);
    assert_eq!(accepted["seal"], id.to_string());
    assert_eq!(accepted["base_revision"], f.base.to_string());
    assert_eq!(accepted["operations"], read.operations().to_string());
    let expected = Object::Snapshot {
        root: Object::Directory(
            Directory::new(vec![Entry {
                name: Name::new(b"src".to_vec()).unwrap(),
                value: EntryRef {
                    mode: EntryMode::File,
                    target: items[0].id().unwrap(),
                },
            }])
            .unwrap(),
        )
        .id()
        .unwrap(),
    };
    assert_eq!(
        accepted["result_snapshot"],
        expected.id().unwrap().to_string()
    );
    let raw = connection(&f.schema).await;
    let workspace: Uuid = raw
        .query_one(
            "SELECT workspace FROM th_agent_runs WHERE run=$1",
            &[&accepted["run"].as_str().unwrap().parse::<Uuid>().unwrap()],
        )
        .await
        .unwrap()
        .get(0);
    f.db.drain_workspace(&ctx(&owner, "drain"), workspace, 1)
        .await
        .unwrap();
    f.db.close_seal_acceptance(*read).await.unwrap();
    f.db = Metadata::new(connection(&f.schema).await).await.unwrap();
    let SealAcceptStart::Replay(replay) = f.db.open_seal_acceptance(&auth, &cmd).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(replay, accepted);
    let mut changed = cmd.clone();
    changed.changeset = Uuid::new_v4();
    assert!(matches!(
        f.db.open_seal_acceptance(&auth, &changed).await,
        Err(Error::IdempotencyConflict)
    ));
    assert!(
        f.db.open_seal_acceptance(&auth, &command(id))
            .await
            .is_err()
    );
    raw.execute("DELETE FROM th_object_holds", &[])
        .await
        .unwrap();
    assert!(
        f.db.try_retire_object(result.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='change.accepted'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_native_validations", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    f.db.revoke_repository_token(&f.scope, owner.token.id, "operator")
        .await
        .unwrap();
    assert!(f.db.open_seal_acceptance(&auth, &cmd).await.is_err());
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; trusted deletion receipt, actual retained graph"]
async fn accepted_seal_survives_workspace_cleanup_and_object_gc() {
    use treeharbor_metadata::cleanup::DeletedFiles;
    let (mut f, owner, node, id, items) = ready().await;
    let cmd = command(id);
    let (auth, read) = open(&mut f, &owner, &cmd).await;
    let (applied, frames) = compute(&f, &items);
    let (ops, result) = publish(&mut f, &read, &frames).await;
    let accepted =
        f.db.finish_seal_acceptance(&read, &applied, &ops, &result)
            .await
            .unwrap();
    f.db.close_seal_acceptance(*read).await.unwrap();
    let raw = connection(&f.schema).await;
    let before: serde_json::Value = raw
        .query_one("SELECT to_jsonb(v) FROM th_change_versions v", &[])
        .await
        .unwrap()
        .get(0);
    let workspace: Uuid = raw
        .query_one("SELECT workspace FROM th_workspaces", &[])
        .await
        .unwrap()
        .get(0);
    f.db.drain_workspace(&ctx(&owner, "drain"), workspace, 1)
        .await
        .unwrap();
    let plan =
        f.db.claim_workspace_cleanup(&node.auth, workspace, node.storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    let proof = DeletedFiles {
        intent_digest: [51; 32],
        receipt_digest: [52; 32],
    };
    f.db.admit_workspace_cleanup(&node.auth, &plan, proof.intent_digest)
        .await
        .unwrap();
    f.db.complete_workspace_cleanup(&node.auth, &plan, &proof)
        .await
        .unwrap();
    raw.execute("DELETE FROM th_object_holds", &[])
        .await
        .unwrap();
    assert_eq!(
        raw.query_one("SELECT to_jsonb(v) FROM th_change_versions v", &[])
            .await
            .unwrap()
            .get::<_, serde_json::Value>(0),
        before
    );
    assert_eq!(
        raw.query_one("SELECT pin_holder FROM th_workspaces", &[])
            .await
            .unwrap()
            .get::<_, Option<String>>(0),
        None
    );
    assert!(
        f.db.try_retire_object(ops.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        f.db.try_retire_object(result.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    // Exercise retention of all real published objects, including blob descendants,
    // after removing temporary upload/read holds and the workspace pin.
    let store = depot(&f);
    let mut retained: Vec<_> = items
        .iter()
        .map(|o| PreparedObject::new(o).unwrap())
        .collect();
    retained.extend(frames);
    for bytes in retained {
        let lease =
            f.db.reserve_object(&store, &bytes, Uuid::new_v4(), 60)
                .await
                .unwrap();
        assert!(!lease.requires_upload());
        assert_eq!(
            store
                .get(lease.key().generation(), bytes.id())
                .unwrap()
                .id()
                .unwrap(),
            bytes.id()
        );
        raw.execute("DELETE FROM th_object_holds", &[])
            .await
            .unwrap();
        assert!(
            f.db.try_retire_object(lease.key(), 0)
                .await
                .unwrap()
                .is_none()
        );
    }
    let SealAcceptStart::Replay(replay) = f.db.open_seal_acceptance(&auth, &cmd).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(replay, accepted);
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; competing owner versions"]
async fn seal_acceptance_version_cas_has_one_winner_and_does_not_reuse_other_request_leases() {
    let (mut f, owner, _, id, items) = ready().await;
    let a = command(id);
    let mut b = a.clone();
    b.key = Uuid::new_v4().to_string();
    let (_, ar) = open(&mut f, &owner, &a).await;
    let (_, br) = open(&mut f, &owner, &b).await;
    let (applied, frames) = compute(&f, &items);
    let (ao, at) = publish(&mut f, &ar, &frames).await;
    assert!(matches!(
        f.db.finish_seal_acceptance(&br, &applied, &ao, &at).await,
        Err(Error::UploadMembership)
    ));
    let (bo, bt) = publish(&mut f, &br, &frames).await;
    let mut other = Metadata::new(connection(&f.schema).await).await.unwrap();
    let (x, y) = tokio::join!(
        f.db.finish_seal_acceptance(&ar, &applied, &ao, &at),
        other.finish_seal_acceptance(&br, &applied, &bo, &bt)
    );
    assert_ne!(x.is_ok(), y.is_ok());
    assert!(matches!(
        x.as_ref().err().or(y.as_ref().err()),
        Some(Error::VersionConflict)
    ));
    f.db.close_seal_acceptance(*ar).await.unwrap();
    f.db.close_seal_acceptance(*br).await.unwrap();
    let mut next = a.clone();
    next.key = Uuid::new_v4().to_string();
    next.previous_version = 1;
    let (_, read) = open(&mut f, &owner, &next).await;
    let (ops, result) = publish(&mut f, &read, &frames).await;
    assert_eq!(
        f.db.finish_seal_acceptance(&read, &applied, &ops, &result)
            .await
            .unwrap()["version"],
        2
    );
    f.db.close_seal_acceptance(*read).await.unwrap();
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; exact end-of-transaction expiry"]
async fn seal_acceptance_deadline_rolls_back_version_pins_receipt_and_outbox() {
    let (mut f, owner, _, id, items) = ready().await;
    let mut cmd = command(id);
    cmd.ttl_seconds = 2;
    let (auth, read) = open(&mut f, &owner, &cmd).await;
    let (applied, frames) = compute(&f, &items);
    let (ops, result) = publish(&mut f, &read, &frames).await;
    let raw = connection(&f.schema).await;
    raw.batch_execute("CREATE FUNCTION seal_accept_delay() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(2.1); RETURN NEW; END $$; CREATE TRIGGER seal_accept_delay AFTER INSERT ON th_outbox FOR EACH ROW WHEN (NEW.kind='change.accepted') EXECUTE FUNCTION seal_accept_delay()").await.unwrap();
    assert!(matches!(
        f.db.finish_seal_acceptance(&read, &applied, &ops, &result)
            .await,
        Err(Error::UploadState)
    ));
    for sql in [
        "SELECT count(*) FROM th_change_versions",
        "SELECT count(*) FROM th_changesets",
        "SELECT count(*) FROM th_object_pins WHERE holder LIKE 'change:%'",
        "SELECT count(*) FROM th_outbox WHERE kind='change.accepted'",
    ] {
        assert_eq!(
            raw.query_one(sql, &[]).await.unwrap().get::<_, i64>(0),
            0,
            "{sql}"
        );
    }
    assert_eq!(
        raw.query_one("SELECT state FROM th_seal_acceptances", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "preparing"
    );
    assert!(matches!(
        f.db.open_seal_acceptance(&auth, &cmd).await,
        Err(Error::UploadState)
    ));
    assert!(
        raw.execute(
            "UPDATE th_seal_acceptances SET expires_at=expires_at+interval '1 minute'",
            &[]
        )
        .await
        .is_err()
    );
    f.db.close_seal_acceptance(*read).await.unwrap();
    raw.batch_execute("DROP TRIGGER seal_accept_delay ON th_outbox")
        .await
        .unwrap();
    cmd.key = Uuid::new_v4().to_string();
    cmd.ttl_seconds = 300;
    let (_, read) = open(&mut f, &owner, &cmd).await;
    let (ops, result) = publish(&mut f, &read, &frames).await;
    assert_eq!(
        f.db.finish_seal_acceptance(&read, &applied, &ops, &result)
            .await
            .unwrap()["version"],
        1
    );
    f.db.close_seal_acceptance(*read).await.unwrap();
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; revoke during computation"]
async fn seal_acceptance_rechecks_current_owner_workspace_epoch_and_pending_identity() {
    for fence in ["grant", "token", "drain", "epoch"] {
        let (mut f, owner, _, id, items) = ready().await;
        let cmd = command(id);
        let (auth, read) = open(&mut f, &owner, &cmd).await;
        let (applied, frames) = compute(&f, &items);
        let (ops, result) = publish(&mut f, &read, &frames).await;
        let mut changed = cmd.clone();
        changed.ttl_seconds += 1;
        assert!(matches!(
            f.db.open_seal_acceptance(&auth, &changed).await,
            Err(Error::IdempotencyConflict)
        ));
        let raw = connection(&f.schema).await;
        match fence {
            "grant" => f
                .db
                .set_repository_access(&f.scope, "owner", &[P::Read, P::WorkspaceWrite], "operator")
                .await
                .unwrap(),
            "token" => {
                f.db.revoke_repository_token(&f.scope, owner.token.id, "operator")
                    .await
                    .unwrap()
            }
            "drain" => {
                let w: Uuid = raw
                    .query_one("SELECT workspace FROM th_workspaces", &[])
                    .await
                    .unwrap()
                    .get(0);
                f.db.drain_workspace(&ctx(&owner, "drain"), w, 1)
                    .await
                    .unwrap();
            }
            "epoch" => {
                raw.execute("UPDATE th_repositories SET epoch=epoch+1", &[])
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            f.db.finish_seal_acceptance(&read, &applied, &ops, &result)
                .await
                .is_err(),
            "{fence}"
        );
        assert_eq!(
            raw.query_one("SELECT count(*) FROM th_change_versions", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            0
        );
        f.db.close_seal_acceptance(*read).await.unwrap();
        cleanup(&f.schema).await;
    }
}
