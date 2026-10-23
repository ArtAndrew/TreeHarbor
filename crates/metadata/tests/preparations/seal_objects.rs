//! Real database publication tests; no claim of real filesystem/runtime capture.
#[path = "seal_acceptance.rs"]
mod acceptance;
use super::*;
use std::collections::BTreeMap;
use treeharbor_core::{ChangeOperation, Entry, EntryMode, EntryRef, Name};
use treeharbor_metadata::{
    reads::MAX_READ_BYTES,
    seals::{SealPlan, SealPublication},
};
fn depot(f: &Fixture) -> ManagedStore {
    ManagedStore::open(
        f._objects.path(),
        TenantId::new(f.scope.tenant()).unwrap(),
        "preparation-tests",
    )
    .unwrap()
}
fn objects(baseline: bool) -> (Vec<Object>, BTreeMap<ObjectId, u8>) {
    let blob = Object::Blob(b"captured bytes".to_vec());
    let mut items = vec![blob.clone()];
    let replacement = if baseline {
        let original = Object::Directory(Directory::new(vec![]).unwrap())
            .id()
            .unwrap();
        let dir = Object::Directory(
            Directory::new(vec![
                Entry {
                    name: Name::new(b"new".to_vec()).unwrap(),
                    value: EntryRef {
                        mode: EntryMode::File,
                        target: blob.id().unwrap(),
                    },
                },
                Entry {
                    name: Name::new(b"keep".to_vec()).unwrap(),
                    value: EntryRef {
                        mode: EntryMode::Directory,
                        target: original,
                    },
                },
            ])
            .unwrap(),
        );
        let replacement = EntryRef {
            mode: EntryMode::Directory,
            target: dir.id().unwrap(),
        };
        items.push(dir);
        replacement
    } else {
        EntryRef {
            mode: EntryMode::File,
            target: blob.id().unwrap(),
        }
    };
    items.push(Object::ChangeOperations(
        Changes::with_subtrees(vec![ChangeOperation {
            path: RepoPath::new(b"src".to_vec()).unwrap(),
            expected: None,
            replacement: Some(replacement),
        }])
        .unwrap(),
    ));
    let deps = if baseline {
        BTreeMap::from([(
            Object::Directory(Directory::new(vec![]).unwrap())
                .id()
                .unwrap(),
            2,
        )])
    } else {
        BTreeMap::new()
    };
    (items, deps)
}
fn declaration(
    items: &[Object],
    deps: BTreeMap<ObjectId, u8>,
    manifest: [u8; 32],
) -> SealPublication {
    let Object::ChangeOperations(changes) = items.last().unwrap() else {
        panic!()
    };
    SealPublication::new(
        manifest,
        changes.clone(),
        items.iter().map(|o| o.id().unwrap()).collect(),
        deps,
        items.iter().map(|o| o.encode().unwrap().len() as u64).sum(),
    )
    .unwrap()
}
async fn setup_capture() -> (Fixture, Owner, Node, SealRequest, SealPlan) {
    let (mut f, owner, node, run) = live().await;
    let request = request(run.run);
    f.db.request_workspace_seal(&ctx(&owner, "seal"), &request)
        .await
        .unwrap();
    removed(&mut f, &node).await;
    let plan = claim_seal(&mut f, &node).await;
    (f, owner, node, request, plan)
}
async fn upload(f: &mut Fixture, node: &Node, plan: &SealPlan, object: &Object) {
    let store = depot(f);
    let frame = PreparedObject::new(object).unwrap();
    let ticket =
        f.db.reserve_seal_object(&node.auth, plan, &store, &frame)
            .await
            .unwrap();
    let receipt = store.put(ticket.generation(), &frame).unwrap();
    f.db.publish_seal_object(&node.auth, plan, &ticket, &receipt)
        .await
        .unwrap();
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; publication is not runtime proof"]
async fn seal_publication_survives_restart_reclaims_and_pins_exact_receipt_once() {
    let (mut f, owner, node, input, old) = setup_capture().await;
    assert!(
        f.db.completed_workspace_seal(&node.auth, &old)
            .await
            .unwrap()
            .is_none()
    );
    let (items, deps) = objects(false);
    let spec = declaration(&items, deps, [17; 32]);
    let store = depot(&f);
    f.db.begin_seal_publication(&node.auth, &old, &store, &spec)
        .await
        .unwrap();
    let frame = PreparedObject::new(&items[0]).unwrap();
    let ticket =
        f.db.reserve_seal_object(&node.auth, &old, &store, &frame)
            .await
            .unwrap();
    let durable = store.put(ticket.generation(), &frame).unwrap();
    // Worker dies after fsync but before the DB records publication.
    let raw = connection(&f.schema).await;
    raw.execute(
        "UPDATE th_workspace_seals SET lease_until=clock_timestamp()-interval '1 second'",
        &[],
    )
    .await
    .unwrap();
    f.db = Metadata::new(connection(&f.schema).await).await.unwrap();
    let plan = claim_seal(&mut f, &node).await;
    assert!(matches!(
        f.db.publish_seal_object(&node.auth, &old, &ticket, &durable)
            .await,
        Err(Error::StaleLease)
    ));
    f.db.begin_seal_publication(&node.auth, &plan, &store, &spec)
        .await
        .unwrap();
    let again =
        f.db.reserve_seal_object(&node.auth, &plan, &store, &frame)
            .await
            .unwrap();
    assert_eq!(again.generation(), ticket.generation());
    f.db.publish_seal_object(&node.auth, &plan, &again, &durable)
        .await
        .unwrap();
    assert!(matches!(
        f.db.finish_seal_publication(&node.auth, &plan, [17; 32])
            .await,
        Err(Error::UploadMembership)
    ));
    upload(&mut f, &node, &plan, &items[1]).await;
    let first =
        f.db.finish_seal_publication(&node.auth, &plan, [17; 32])
            .await
            .unwrap();
    assert_eq!(first["state"], "ready");
    assert_eq!(
        f.db.completed_workspace_seal(&node.auth, &plan)
            .await
            .unwrap(),
        Some(first.clone())
    );
    assert!(matches!(
        f.db.completed_workspace_seal(&node.auth, &old).await,
        Err(Error::StaleLease)
    ));
    assert_eq!(first["operations"], items[1].id().unwrap().to_string());
    // A lost completion response is replayed even after normal workspace drain.
    f.db.drain_workspace(
        &ctx(&owner, "drain"),
        plan.context().prepared().workspace,
        1,
    )
    .await
    .unwrap();
    f.db = Metadata::new(connection(&f.schema).await).await.unwrap();
    assert_eq!(
        f.db.finish_seal_publication(&node.auth, &plan, [17; 32])
            .await
            .unwrap(),
        first
    );
    assert!(matches!(
        f.db.finish_seal_publication(&node.auth, &old, [17; 32])
            .await,
        Err(Error::StaleLease)
    ));
    assert!(matches!(
        f.db.finish_seal_publication(&node.auth, &plan, [18; 32])
            .await,
        Err(Error::ReceiptMismatch)
    ));
    assert_eq!(
        f.db.completed_workspace_seal(&node.auth, &plan)
            .await
            .unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        f.db.workspace_seal_status(&owner.read, input.seal)
            .await
            .unwrap()["state"],
        "ready"
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='workspace.seal.ready'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    assert_eq!(
        raw.query_one("SELECT reserved_objects FROM th_seal_artifacts", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    raw.execute("DELETE FROM th_object_holds", &[])
        .await
        .unwrap();
    for object in &items {
        let lease =
            f.db.reserve_object(
                &store,
                &PreparedObject::new(object).unwrap(),
                Uuid::new_v4(),
                60,
            )
            .await
            .unwrap();
        f.db.release_object_hold(&lease).await.unwrap();
        assert!(
            f.db.try_retire_object(lease.key(), 0)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(
        raw.execute("UPDATE th_seal_artifacts SET receipt='{}'::jsonb", &[])
            .await
            .is_err()
    );
    assert!(
        raw.execute("UPDATE th_seal_items SET complete=false", &[])
            .await
            .is_err()
    );
    f.db.revoke_repository_token(&f.scope, node.token.id, "operator")
        .await
        .unwrap();
    assert!(
        f.db.finish_seal_publication(&node.auth, &plan, [17; 32])
            .await
            .is_err()
    );
    assert!(
        f.db.completed_workspace_seal(&node.auth, &plan)
            .await
            .is_err()
    );
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn seal_baseline_requires_exact_snapshot_proof_and_child_first_uploads() {
    let (mut f, _, node, _, plan) = setup_capture().await;
    let (items, deps) = objects(true);
    let spec = declaration(&items, deps.clone(), [20; 32]);
    let store = depot(&f);
    f.db.begin_seal_publication(&node.auth, &plan, &store, &spec)
        .await
        .unwrap();
    let dir = PreparedObject::new(&items[1]).unwrap();
    assert!(matches!(
        f.db.reserve_seal_object(&node.auth, &plan, &store, &dir)
            .await,
        Err(Error::UploadMembership)
    ));
    upload(&mut f, &node, &plan, &items[0]).await;
    // Existing shared CAS objects are insufficient without proof from this source.
    assert!(matches!(
        f.db.reserve_seal_object(&node.auth, &plan, &store, &dir)
            .await,
        Err(Error::UploadMembership)
    ));
    let source = f.db.open_seal_source(&node.auth, &plan).await.unwrap();
    let root =
        f.db.read_seal_source(
            &node.auth,
            &plan,
            &source,
            &store,
            &[source.root()],
            MAX_READ_BYTES,
        )
        .await
        .unwrap()
        .read()
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(root.object().kind(), 3);
    assert_eq!(root.id(), plan.context().prepared().snapshot);
    let child = root.children().next().unwrap();
    let proof =
        f.db.read_seal_source(&node.auth, &plan, &source, &store, &[child], MAX_READ_BYTES)
            .await
            .unwrap()
            .read()
            .unwrap()
            .pop()
            .unwrap();
    assert!(deps.contains_key(&proof.id()));
    let foreign_session = f.db.open_seal_source(&node.auth, &plan).await.unwrap();
    assert!(matches!(
        f.db.prove_seal_dependency(&node.auth, &plan, &foreign_session, &proof)
            .await,
        Err(Error::ReadScope)
    ));
    f.db.close_seal_source(foreign_session).await.unwrap();
    f.db.prove_seal_dependency(&node.auth, &plan, &source, &proof)
        .await
        .unwrap();
    f.db.prove_seal_dependency(&node.auth, &plan, &source, &proof)
        .await
        .unwrap();
    f.db.close_seal_source(source).await.unwrap();
    for object in &items[1..] {
        upload(&mut f, &node, &plan, object).await;
    }
    assert_eq!(
        f.db.finish_seal_publication(&node.auth, &plan, [20; 32])
            .await
            .unwrap()["state"],
        "ready"
    );
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn seal_rejects_changed_manifest_backend_paths_unlisted_objects_and_unused_objects() {
    let (mut f, _, node, _, plan) = setup_capture().await;
    let (items, deps) = objects(false);
    let store = depot(&f);
    let mut wrong = items.clone();
    wrong[1] = Object::ChangeOperations(
        Changes::new(vec![ChangeOperation {
            path: RepoPath::new(b"unrequested".to_vec()).unwrap(),
            expected: None,
            replacement: Some(EntryRef {
                mode: EntryMode::File,
                target: items[0].id().unwrap(),
            }),
        }])
        .unwrap(),
    );
    assert!(matches!(
        f.db.begin_seal_publication(
            &node.auth,
            &plan,
            &store,
            &declaration(&wrong, deps.clone(), [30; 32])
        )
        .await,
        Err(Error::ReceiptMismatch)
    ));
    let spec = declaration(&items, deps.clone(), [30; 32]);
    f.db.begin_seal_publication(&node.auth, &plan, &store, &spec)
        .await
        .unwrap();
    assert!(matches!(
        f.db.begin_seal_publication(
            &node.auth,
            &plan,
            &store,
            &declaration(&items, deps, [31; 32])
        )
        .await,
        Err(Error::ReceiptMismatch)
    ));
    let other = ManagedStore::open(
        f._objects.path(),
        TenantId::new(f.scope.tenant()).unwrap(),
        "other-backend",
    )
    .unwrap();
    assert!(matches!(
        f.db.begin_seal_publication(&node.auth, &plan, &other, &spec)
            .await,
        Err(Error::ReceiptMismatch)
    ));
    let unlisted = PreparedObject::new(&Object::Blob(b"not declared".to_vec())).unwrap();
    assert!(matches!(
        f.db.reserve_seal_object(&node.auth, &plan, &store, &unlisted)
            .await,
        Err(Error::UploadMembership)
    ));
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one("SELECT reserved_objects FROM th_seal_artifacts", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(
        raw.execute(
            "UPDATE th_seal_artifacts SET manifest=repeat('x',32)::bytea",
            &[]
        )
        .await
        .is_err()
    );
    cleanup(&f.schema).await;
    // Valid hashes and successful uploads still cannot hide unrelated objects.
    let (mut f, _, node, _, plan) = setup_capture().await;
    let (mut items, deps) = objects(false);
    items.insert(0, Object::Blob(b"extraneous".to_vec()));
    let store = depot(&f);
    f.db.begin_seal_publication(
        &node.auth,
        &plan,
        &store,
        &declaration(&items, deps, [32; 32]),
    )
    .await
    .unwrap();
    for object in &items {
        upload(&mut f, &node, &plan, object).await;
    }
    assert!(matches!(
        f.db.finish_seal_publication(&node.auth, &plan, [32; 32])
            .await,
        Err(Error::UploadMembership)
    ));
    assert_eq!(
        connection(&f.schema)
            .await
            .query_one(
                "SELECT count(*) FROM th_object_pins WHERE holder LIKE '%/operations'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn seal_cancel_and_budget_failures_never_leave_ready_pins_or_receipt() {
    let (mut f, owner, node, input, plan) = setup_capture().await;
    let (items, deps) = objects(false);
    let Object::ChangeOperations(changes) = items.last().unwrap() else {
        panic!()
    };
    let bad = SealPublication::new(
        [40; 32],
        changes.clone(),
        items.iter().map(|o| o.id().unwrap()).collect(),
        deps,
        22,
    )
    .unwrap();
    let store = depot(&f);
    f.db.begin_seal_publication(&node.auth, &plan, &store, &bad)
        .await
        .unwrap();
    assert!(matches!(
        f.db.reserve_seal_object(
            &node.auth,
            &plan,
            &store,
            &PreparedObject::new(&items[0]).unwrap()
        )
        .await,
        Err(Error::UploadLimit)
    ));
    f.db.cancel_workspace_seal(&ctx(&owner, "cancel"), input.seal)
        .await
        .unwrap();
    assert!(matches!(
        f.db.finish_seal_publication(&node.auth, &plan, [40; 32])
            .await,
        Err(Error::StaleLease)
    ));
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one(
            "SELECT reserved_objects,receipt IS NULL FROM th_seal_artifacts",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='workspace.seal.ready'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; expiry during finalization transaction"]
async fn seal_completion_lease_expiring_inside_trigger_rolls_back_every_effect() {
    let (mut f, owner, node, input, plan) = setup_capture().await;
    let (items, deps) = objects(false);
    let store = depot(&f);
    f.db.begin_seal_publication(
        &node.auth,
        &plan,
        &store,
        &declaration(&items, deps, [50; 32]),
    )
    .await
    .unwrap();
    for object in &items {
        upload(&mut f, &node, &plan, object).await;
    }
    let raw = connection(&f.schema).await;
    raw.execute(
        "UPDATE th_workspace_seals SET lease_until=clock_timestamp()+interval '1 second'",
        &[],
    )
    .await
    .unwrap();
    raw.batch_execute("CREATE FUNCTION seal_finish_delay() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(1.2); RETURN NEW; END $$; CREATE TRIGGER seal_finish_delay AFTER INSERT ON th_object_pins FOR EACH ROW EXECUTE FUNCTION seal_finish_delay()").await.unwrap();
    assert!(matches!(
        f.db.finish_seal_publication(&node.auth, &plan, [50; 32])
            .await,
        Err(Error::StaleLease)
    ));
    assert_eq!(
        f.db.workspace_seal_status(&owner.read, input.seal)
            .await
            .unwrap()["state"],
        "capturing"
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_pins WHERE holder LIKE '%/operations'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert!(
        raw.query_one("SELECT receipt IS NULL FROM th_seal_artifacts", &[])
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='workspace.seal.ready'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    raw.batch_execute("DROP TRIGGER seal_finish_delay ON th_object_pins")
        .await
        .unwrap();
    let current = claim_seal(&mut f, &node).await;
    assert_eq!(
        f.db.finish_seal_publication(&node.auth, &current, [50; 32])
            .await
            .unwrap()["state"],
        "ready"
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; all final mutable authorization fences"]
async fn seal_ready_never_bypasses_owner_node_or_workspace_revocation() {
    for fence in [
        "owner_disabled",
        "owner_grant",
        "node_token",
        "node_grant",
        "drain",
        "epoch",
        "freeze",
    ] {
        let (mut f, owner, node, _, plan) = setup_capture().await;
        let (items, deps) = objects(false);
        let store = depot(&f);
        f.db.begin_seal_publication(
            &node.auth,
            &plan,
            &store,
            &declaration(&items, deps, [61; 32]),
        )
        .await
        .unwrap();
        for object in &items {
            upload(&mut f, &node, &plan, object).await;
        }
        let raw = connection(&f.schema).await;
        match fence {
            "owner_disabled" => {
                raw.execute(
                    "UPDATE th_principals SET enabled=false WHERE principal='owner'",
                    &[],
                )
                .await
                .unwrap();
            }
            "owner_grant" => {
                f.db.set_repository_access(&f.scope, "owner", &[P::Read], "operator")
                    .await
                    .unwrap()
            }
            "node_token" => {
                f.db.revoke_repository_token(&f.scope, node.token.id, "operator")
                    .await
                    .unwrap()
            }
            "node_grant" => {
                f.db.set_repository_access(&f.scope, "node", &[P::Read], "operator")
                    .await
                    .unwrap()
            }
            "drain" => {
                f.db.drain_workspace(
                    &ctx(&owner, "drain"),
                    plan.context().prepared().workspace,
                    1,
                )
                .await
                .unwrap();
            }
            "epoch" => {
                raw.execute("UPDATE th_repositories SET epoch=epoch+1", &[])
                    .await
                    .unwrap();
            }
            "freeze" => {
                raw.execute("UPDATE th_repositories SET state='frozen'", &[])
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            f.db.finish_seal_publication(&node.auth, &plan, [61; 32])
                .await
                .is_err(),
            "{fence}"
        );
        assert!(
            raw.query_one("SELECT receipt IS NULL FROM th_seal_artifacts", &[])
                .await
                .unwrap()
                .get::<_, bool>(0),
            "{fence}"
        );
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_object_pins WHERE holder LIKE '%/operations'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0,
            "{fence}"
        );
        cleanup(&f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; shared CAS is not source membership"]
async fn seal_cannot_borrow_baseline_from_another_repository() {
    let (mut f, _, node, _, plan) = setup_capture().await;
    let store = depot(&f);
    let foreign = Scope::new(f.scope.tenant(), Uuid::new_v4()).unwrap();
    f.db.create_repository(&foreign, Authority::Standalone)
        .await
        .unwrap();
    let secret = Object::Blob(b"foreign repository private content".to_vec());
    let frame = PreparedObject::new(&secret).unwrap();
    let lease =
        f.db.reserve_object(&store, &frame, Uuid::new_v4(), 60)
            .await
            .unwrap();
    f.db.publish_object(
        &lease,
        &store.put(lease.key().generation(), &frame).unwrap(),
    )
    .await
    .unwrap();
    let request = treeharbor_metadata::Request::new(
        foreign.clone(),
        "pin",
        "operator",
        "workspace.create.v1",
        Authority::Standalone,
        1,
        b"fixture",
    )
    .unwrap();
    let Begin::Fresh(mutation) = f.db.begin(request).await.unwrap() else {
        panic!()
    };
    mutation.pin_object("foreign-root", &lease).await.unwrap();
    mutation.complete(json!({}), vec![]).await.unwrap();
    let operations = Object::ChangeOperations(
        Changes::new(vec![ChangeOperation {
            path: RepoPath::new(b"src".to_vec()).unwrap(),
            expected: None,
            replacement: Some(EntryRef {
                mode: EntryMode::File,
                target: secret.id().unwrap(),
            }),
        }])
        .unwrap(),
    );
    let spec = declaration(
        std::slice::from_ref(&operations),
        BTreeMap::from([(secret.id().unwrap(), 1)]),
        [62; 32],
    );
    f.db.begin_seal_publication(&node.auth, &plan, &store, &spec)
        .await
        .unwrap();
    let foreign_read =
        f.db.open_read_session(&foreign, "foreign-root", 60)
            .await
            .unwrap();
    let proof =
        f.db.open_read_batch(
            &foreign_read,
            &store,
            &[foreign_read.root()],
            MAX_READ_BYTES,
        )
        .await
        .unwrap()
        .read()
        .unwrap()
        .pop()
        .unwrap();
    let source = f.db.open_seal_source(&node.auth, &plan).await.unwrap();
    assert!(matches!(
        f.db.prove_seal_dependency(&node.auth, &plan, &source, &proof)
            .await,
        Err(Error::ReadScope)
    ));
    assert!(matches!(
        f.db.read_seal_source(
            &node.auth,
            &plan,
            &source,
            &store,
            &[foreign_read.root()],
            MAX_READ_BYTES
        )
        .await,
        Err(Error::ReadScope)
    ));
    assert!(matches!(
        f.db.reserve_seal_object(
            &node.auth,
            &plan,
            &store,
            &PreparedObject::new(&operations).unwrap()
        )
        .await,
        Err(Error::UploadMembership)
    ));
    assert!(matches!(
        f.db.finish_seal_publication(&node.auth, &plan, [62; 32])
            .await,
        Err(Error::UploadMembership)
    ));
    f.db.close_read_session(foreign_read).await.unwrap();
    f.db.close_seal_source(source).await.unwrap();
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_seal_items WHERE complete", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    cleanup(&f.schema).await;
}
