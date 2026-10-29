#[path = "integration/candidate_review.rs"]
mod candidate_review;
#[path = "integration/rebases.rs"]
mod rebases;
#[path = "integration/review.rs"]
mod review;
use super::*;
use treeharbor_merge::{CandidateOutcome, Limits, PreparedCandidate, prepare_candidate};
use treeharbor_metadata::{
    access::{Authorization, IssuedToken, Permission as P, PrincipalKind},
    integration::{CheckDefinition, Command, Policy},
};

fn cmd(key: &str) -> Command {
    Command {
        key: key.into(),
        epoch: 1,
    }
}
struct Actors {
    admin: IssuedToken,
    owner: IssuedToken,
    worker: IssuedToken,
    reviewer: IssuedToken,
    validator: IssuedToken,
}
async fn actor(f: &mut Fixture, name: &str, kind: PrincipalKind, permissions: &[P]) -> IssuedToken {
    f.db.provision_principal(f.scope.tenant(), name, kind, "operator")
        .await
        .unwrap();
    f.db.set_repository_access(&f.scope, name, permissions, "operator")
        .await
        .unwrap();
    f.db.issue_repository_token(&f.scope, name, permissions, 3600, "operator")
        .await
        .unwrap()
}
async fn auth(f: &mut Fixture, token: &IssuedToken, permission: P) -> Authorization {
    f.db.authorize(&token.secret, &f.scope, permission)
        .await
        .unwrap()
}
fn policy() -> Policy {
    let digest = ObjectId::from_bytes([7; 32]).to_string();
    Policy {
        version: 1,
        checks: vec![CheckDefinition {
            name: "tests".into(),
            definition: digest.clone(),
            environment: digest.clone(),
            toolchain: digest.clone(),
            dependencies: digest,
            validators: BTreeSet::from(["ci".into()]),
        }],
        reviewers: BTreeSet::from(["reviewer".into()]),
        approvals: 1,
    }
}
async fn controls(f: &mut Fixture) -> Actors {
    let actors = Actors {
        admin: actor(
            f,
            "admin",
            PrincipalKind::Human,
            &[P::Read, P::RepositoryWrite, P::PolicyWrite],
        )
        .await,
        owner: actor(
            f,
            "agent-owner",
            PrincipalKind::Agent,
            &[P::Read, P::ChangeWrite, P::QueueRequest],
        )
        .await,
        worker: actor(
            f,
            "integrator",
            PrincipalKind::Integrator,
            &[P::Read, P::IntegrationControl],
        )
        .await,
        reviewer: actor(f, "reviewer", PrincipalKind::Human, &[P::Read, P::Review]).await,
        validator: actor(
            f,
            "ci",
            PrincipalKind::Validator,
            &[P::Read, P::ValidationReport],
        )
        .await,
    };
    let p = policy();
    let admin = auth(f, &actors.admin, P::PolicyWrite).await;
    f.db.put_native_policy(&admin, &cmd("policy"), &p)
        .await
        .unwrap();
    let admin = auth(f, &actors.admin, P::RepositoryWrite).await;
    let RevisionRef::Native(head) = f.revision else {
        panic!()
    };
    f.db.create_native_ref(
        &admin,
        &cmd("ref"),
        "main",
        head,
        ObjectId::from_bytes(p.digest().unwrap()),
    )
    .await
    .unwrap();
    actors
}
// Frozen fixture for the published v9 first-version intake shape. Migration
// tests populate the actual old schema; current intake requires schema 032.
// This does not claim execution by an old binary or test current admission.
async fn legacy_change(
    f: &mut Fixture,
    key: &str,
    change: Uuid,
    prepared: &(AppliedChange, ObjectLease, ObjectLease),
) {
    let (applied, operations, result) = prepared;
    let source =
        f.db.lease_pinned_root(
            &f.scope,
            &format!("workspace:{}", f.workspace),
            Uuid::new_v4(),
            60,
        )
        .await
        .unwrap();
    let request = treeharbor_metadata::Request::new(
        f.scope.clone(),
        key,
        "agent-owner",
        "change.accept.v1",
        Authority::Standalone,
        1,
        json!([
            1,
            change.to_string(),
            0,
            f.run.to_string(),
            1,
            applied.base_snapshot().to_string(),
            applied.operations().to_string(),
            applied.result_snapshot().to_string()
        ])
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let treeharbor_metadata::Begin::Fresh(tx) = f.db.begin(request).await.unwrap() else {
        panic!("legacy fixture already exists")
    };
    let prefix = format!("change:{change}:1");
    let base_pin = format!("{prefix}:base");
    let operations_pin = format!("{prefix}:operations");
    let result_pin = format!("{prefix}:result");
    tx.pin_object(&base_pin, &source).await.unwrap();
    tx.pin_object(&operations_pin, operations).await.unwrap();
    tx.pin_object(&result_pin, result).await.unwrap();
    tx.transaction().execute("INSERT INTO th_changesets(tenant,repository,changeset,principal,latest_version) VALUES($1,$2,$3,'agent-owner',1)",&[&f.scope.tenant(),&f.scope.repository(),&change]).await.unwrap();
    tx.transaction().execute("INSERT INTO th_change_versions(tenant,repository,changeset,version,run,base_revision,base_snapshot,operations,result_snapshot,base_pin,operations_pin,result_pin,operation_key) VALUES($1,$2,$3,1,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
        &[&f.scope.tenant(),&f.scope.repository(),&change,&f.run,&f.revision.to_string(),&applied.base_snapshot().as_bytes().as_slice(),&applied.operations().as_bytes().as_slice(),&applied.result_snapshot().as_bytes().as_slice(),&base_pin,&operations_pin,&result_pin,&key]).await.unwrap();
    let value = json!({"changeset":change.to_string(),"version":1,"state":"accepted","run":f.run.to_string(),
        "base_revision":f.revision.to_string(),"base_snapshot":applied.base_snapshot().to_string(),
        "operations":applied.operations().to_string(),"result_snapshot":applied.result_snapshot().to_string()});
    tx.complete(
        value.clone(),
        vec![treeharbor_metadata::Event {
            kind: "change.accepted".into(),
            payload: value,
        }],
    )
    .await
    .unwrap();
}

async fn legacy_proposal(f: &mut Fixture, actors: &Actors, key: &str, content: &[u8]) -> Uuid {
    let change = Uuid::new_v4();
    let prepared = f.prepare(content).await;
    legacy_change(f, &format!("accept-{key}"), change, &prepared).await;
    let owner = auth(f, &actors.owner, P::QueueRequest).await;
    let entry = Uuid::new_v4();
    f.db.enqueue_native_change(
        &owner,
        &cmd(&format!("enqueue-{key}")),
        entry,
        "main",
        change,
        1,
    )
    .await
    .unwrap();
    entry
}

async fn proposal(f: &mut Fixture, actors: &Actors, key: &str, content: &[u8]) -> Uuid {
    let change = Uuid::new_v4();
    let prepared = f.prepare(content).await;
    f.db.accept_change(
        &context(&f.scope, &format!("accept-{key}")),
        &submission(change, f.run, &prepared),
    )
    .await
    .unwrap();
    let owner = auth(f, &actors.owner, P::QueueRequest).await;
    let entry = Uuid::new_v4();
    f.db.enqueue_native_change(
        &owner,
        &cmd(&format!("enqueue-{key}")),
        entry,
        "main",
        change,
        1,
    )
    .await
    .unwrap();
    entry
}
async fn start(f: &mut Fixture, actors: &Actors, entry: Uuid) -> (Uuid, Authorization) {
    let worker = auth(f, &actors.worker, P::IntegrationControl).await;
    let candidate = Uuid::new_v4();
    f.db.start_native_candidate(
        &worker,
        &cmd(&format!("start-{candidate}")),
        entry,
        candidate,
    )
    .await
    .unwrap();
    (candidate, worker)
}
async fn prepare(f: &mut Fixture, worker: &Authorization, candidate: Uuid) -> PreparedCandidate {
    let read = f.db.open_native_candidate(worker, candidate).await.unwrap();
    for (index, node) in read.roots().into_iter().enumerate() {
        let batch =
            f.db.read_native_candidate(&read, index, &f.depot, &[node], 1_000_000)
                .await
                .unwrap();
        assert_eq!(batch.read().unwrap().len(), 1);
    }
    let CandidateOutcome::Ready(prepared) =
        prepare_candidate(&f.stage, read.input(), Limits::default()).unwrap()
    else {
        panic!()
    };
    let mut visited = BTreeSet::new();
    let mut pending = vec![(prepared.revision(), false)];
    let mut root = None;
    while let Some((id, done)) = pending.pop() {
        if visited.contains(&id) {
            continue;
        }
        let object = f.stage.get(id).unwrap();
        if !done {
            pending.push((id, true));
            pending.extend(object.references().into_iter().map(|(id, _)| (id, false)));
            continue;
        }
        let object = PreparedObject::new(&object).unwrap();
        let lease =
            f.db.reserve_native_candidate_object(worker, candidate, &f.depot, &object)
                .await
                .unwrap();
        // A retry must own the same physical generation and deterministic hold.
        let replay =
            f.db.reserve_native_candidate_object(worker, candidate, &f.depot, &object)
                .await
                .unwrap();
        assert_eq!(lease.key().generation(), replay.key().generation());
        assert_eq!(lease.token(), replay.token());
        if lease.requires_upload() {
            let receipt = f.depot.put(lease.key().generation(), &object).unwrap();
            f.db.publish_object(&lease, &receipt).await.unwrap();
        }
        visited.insert(id);
        if id == prepared.revision() {
            root = Some(lease);
        }
    }
    f.db.finish_native_candidate(
        worker,
        &cmd(&format!("finish-{candidate}")),
        candidate,
        &prepared,
        root.as_ref().unwrap(),
    )
    .await
    .unwrap();
    f.db.close_native_candidate_read(read).await.unwrap();
    *prepared
}
async fn evidence(
    f: &mut Fixture,
    actors: &Actors,
    worker: &Authorization,
    candidate: Uuid,
) -> (Uuid, Uuid) {
    let run = Uuid::new_v4();
    f.db.assign_native_check(
        worker,
        &cmd(&format!("assign-{run}")),
        candidate,
        "tests",
        run,
        "ci",
    )
    .await
    .unwrap();
    let validator = auth(f, &actors.validator, P::ValidationReport).await;
    f.db.report_native_validation(
        &validator,
        &cmd(&format!("report-{run}")),
        run,
        Uuid::new_v4(),
        "passed",
        ObjectId::from_bytes([9; 32]),
    )
    .await
    .unwrap();
    let reviewer = auth(f, &actors.reviewer, P::Review).await;
    let review = Uuid::new_v4();
    f.db.record_native_review(
        &reviewer,
        &cmd(&format!("review-{review}")),
        candidate,
        review,
        "approve",
    )
    .await
    .unwrap();
    (run, review)
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn native_candidate_requires_exact_checks_and_review_then_publishes_one_durable_ref_update() {
    let mut f = Fixture::new().await;
    let actors = controls(&mut f).await;
    let entry = proposal(&mut f, &actors, "a", b"native agent output").await;
    let (candidate, worker) = start(&mut f, &actors, entry).await;
    let prepared = prepare(&mut f, &worker, candidate).await;
    assert!(matches!(
        f.db.land_native_candidate(&worker, &cmd("land"), candidate)
            .await,
        Err(MetaError::AdmissionBlocked)
    ));
    evidence(&mut f, &actors, &worker, candidate).await;
    let result =
        f.db.land_native_candidate(&worker, &cmd("land"), candidate)
            .await
            .unwrap();
    assert_eq!(result["state"], "landed");
    assert_eq!(
        result["revision"],
        RevisionRef::Native(prepared.revision()).to_string()
    );
    let mut restarted = Metadata::new(connection(&f.schema).await).await.unwrap();
    assert_eq!(
        restarted
            .land_native_candidate(&worker, &cmd("land"), candidate)
            .await
            .unwrap(),
        result
    );
    assert_eq!(
        restarted
            .land_native_candidate(&worker, &cmd("land-second-key"), candidate)
            .await
            .unwrap(),
        result
    );
    let reader = auth(&mut f, &actors.owner, P::Read).await;
    let state = f.db.resolve_native_ref(&reader, "main").await.unwrap();
    assert_eq!(state["head"], result["revision"]);
    assert_eq!(state["generation"], 2);
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_native_ref_updates WHERE kind='land'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='native.ref.published'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    let graph = treeharbor_core::validate_graph(&f.stage, prepared.revision(), 100).unwrap();
    let RevisionRef::Native(base) = f.revision else {
        panic!()
    };
    assert!(graph.contains(&base));
    assert!(graph.contains(&prepared.proposal_revision()));
    // Expire every temporary hold; only durable graph edges and pins may keep
    // the full published ancestry alive, including the proposal's parent.
    raw.execute(
        "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second'",
        &[],
    )
    .await
    .unwrap();
    for id in &graph {
        let object = PreparedObject::new(&f.stage.get(*id).unwrap()).unwrap();
        let lease =
            f.db.reserve_object(&f.depot, &object, Uuid::new_v4(), 60)
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
        raw.query_one(
            "SELECT EXISTS(SELECT 1 FROM th_object_pins WHERE holder LIKE 'native-published:%')",
            &[]
        )
        .await
        .unwrap()
        .get::<_, bool>(0)
    );
    drop(raw);
    drop(restarted);
    drop(f.db);
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn native_rescheduled_checks_revocations_and_policy_changes_invalidate_old_green() {
    let mut f = Fixture::new().await;
    let actors = controls(&mut f).await;
    let entry = proposal(&mut f, &actors, "a", b"output").await;
    let (candidate, worker) = start(&mut f, &actors, entry).await;
    prepare(&mut f, &worker, candidate).await;
    let (old_run, review) = evidence(&mut f, &actors, &worker, candidate).await;
    let next_run = Uuid::new_v4();
    f.db.assign_native_check(&worker, &cmd("rerun"), candidate, "tests", next_run, "ci")
        .await
        .unwrap();
    assert!(matches!(
        f.db.land_native_candidate(&worker, &cmd("land"), candidate)
            .await,
        Err(MetaError::AdmissionBlocked)
    ));
    let validator = auth(&mut f, &actors.validator, P::ValidationReport).await;
    assert!(matches!(
        f.db.report_native_validation(
            &validator,
            &cmd("old-run"),
            old_run,
            Uuid::new_v4(),
            "passed",
            ObjectId::from_bytes([9; 32])
        )
        .await,
        Err(MetaError::IntegrationState)
    ));
    f.db.report_native_validation(
        &validator,
        &cmd("new-run"),
        next_run,
        Uuid::new_v4(),
        "passed",
        ObjectId::from_bytes([9; 32]),
    )
    .await
    .unwrap();
    let reviewer = auth(&mut f, &actors.reviewer, P::Review).await;
    f.db.revoke_native_review(&reviewer, &cmd("revoke-review"), review)
        .await
        .unwrap();
    assert!(matches!(
        f.db.land_native_candidate(&worker, &cmd("land"), candidate)
            .await,
        Err(MetaError::AdmissionBlocked)
    ));
    f.db.record_native_review(
        &reviewer,
        &cmd("new-review"),
        candidate,
        Uuid::new_v4(),
        "approve",
    )
    .await
    .unwrap();
    f.db.revoke_native_validation(&validator, &cmd("revoke-run"), next_run)
        .await
        .unwrap();
    assert!(matches!(
        f.db.land_native_candidate(&worker, &cmd("land"), candidate)
            .await,
        Err(MetaError::AdmissionBlocked)
    ));
    let admin = auth(&mut f, &actors.admin, P::PolicyWrite).await;
    let digest = ObjectId::from_bytes(policy().digest().unwrap());
    f.db.change_native_policy(&admin, &cmd("reactivate-policy"), "main", 1, digest)
        .await
        .unwrap();
    assert!(matches!(
        f.db.land_native_candidate(&worker, &cmd("land"), candidate)
            .await,
        Err(MetaError::IntegrationState)
    ));
    let reader = auth(&mut f, &actors.owner, P::Read).await;
    assert_eq!(
        f.db.native_queue_status(&reader, entry).await.unwrap()["state"],
        "stale"
    );
    assert_eq!(
        f.db.resolve_native_ref(&reader, "main").await.unwrap()["head"],
        f.revision.to_string()
    );
    drop(f.db);
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; authorization and evidence ambiguity"]
async fn native_untrusted_or_competing_evidence_and_revoked_credentials_cannot_land() {
    let mut f = Fixture::new().await;
    let actors = controls(&mut f).await;
    assert!(matches!(
        f.db.set_repository_access(&f.scope, "agent-owner", &[P::ValidationReport], "operator")
            .await,
        Err(MetaError::Forbidden)
    ));
    let entry = proposal(&mut f, &actors, "a", b"output").await;
    let (id, worker) = start(&mut f, &actors, entry).await;
    prepare(&mut f, &worker, id).await;
    let agent = auth(&mut f, &actors.owner, P::QueueRequest).await;
    assert!(matches!(
        f.db.land_native_candidate(&agent, &cmd("agent-land"), id)
            .await,
        Err(MetaError::Forbidden)
    ));
    let (run, _) = evidence(&mut f, &actors, &worker, id).await;
    assert!(matches!(
        f.db.report_native_validation(
            &agent,
            &cmd("agent-green"),
            run,
            Uuid::new_v4(),
            "passed",
            ObjectId::from_bytes([9; 32])
        )
        .await,
        Err(MetaError::Forbidden)
    ));
    let validator = auth(&mut f, &actors.validator, P::ValidationReport).await;
    f.db.report_native_validation(
        &validator,
        &cmd("duplicate-definitive"),
        run,
        Uuid::new_v4(),
        "passed",
        ObjectId::from_bytes([9; 32]),
    )
    .await
    .unwrap();
    assert!(matches!(
        f.db.land_native_candidate(&worker, &cmd("land"), id).await,
        Err(MetaError::AdmissionBlocked)
    ));
    let next = Uuid::new_v4();
    f.db.assign_native_check(&worker, &cmd("reschedule"), id, "tests", next, "ci")
        .await
        .unwrap();
    let receipt = Uuid::new_v4();
    let reported =
        f.db.report_native_validation(
            &validator,
            &cmd("new-green"),
            next,
            receipt,
            "passed",
            ObjectId::from_bytes([9; 32]),
        )
        .await
        .unwrap();
    assert_eq!(
        reported,
        f.db.report_native_validation(
            &validator,
            &cmd("new-green"),
            next,
            receipt,
            "passed",
            ObjectId::from_bytes([9; 32])
        )
        .await
        .unwrap()
    );
    assert!(matches!(
        f.db.report_native_validation(
            &validator,
            &cmd("new-green"),
            next,
            receipt,
            "failed",
            ObjectId::from_bytes([9; 32])
        )
        .await,
        Err(MetaError::IdempotencyConflict)
    ));
    f.db.revoke_repository_token(&f.scope, actors.validator.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.land_native_candidate(&worker, &cmd("land"), id).await,
        Err(MetaError::AdmissionBlocked)
    ));
    assert!(
        f.db.report_native_validation(
            &validator,
            &cmd("revoked-report"),
            next,
            Uuid::new_v4(),
            "passed",
            ObjectId::from_bytes([9; 32])
        )
        .await
        .is_err()
    );
    let reader = auth(&mut f, &actors.owner, P::Read).await;
    assert_eq!(
        f.db.resolve_native_ref(&reader, "main").await.unwrap()["head"],
        f.revision.to_string()
    );
    drop(f.db);
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; competing workers and serial ordering"]
async fn native_two_workers_land_once_and_next_entry_uses_published_head() {
    let mut f = Fixture::new().await;
    let actors = controls(&mut f).await;
    let first = proposal(&mut f, &actors, "a", b"a").await;
    let second = proposal(&mut f, &actors, "b", b"b").await;
    let worker = auth(&mut f, &actors.worker, P::IntegrationControl).await;
    assert!(matches!(
        f.db.start_native_candidate(&worker, &cmd("out-of-order"), second, Uuid::new_v4())
            .await,
        Err(MetaError::IntegrationState)
    ));
    let (id, worker) = start(&mut f, &actors, first).await;
    let prepared = prepare(&mut f, &worker, id).await;
    evidence(&mut f, &actors, &worker, id).await;
    let mut other = Metadata::new(connection(&f.schema).await).await.unwrap();
    let one = cmd("land-one");
    let two = cmd("land-two");
    let (a, b) = tokio::join!(
        f.db.land_native_candidate(&worker, &one, id),
        other.land_native_candidate(&worker, &two, id)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    let (next, worker) = start(&mut f, &actors, second).await;
    let read = f.db.open_native_candidate(&worker, next).await.unwrap();
    assert_eq!(read.input().current_revision, prepared.revision());
    assert!(matches!(
        prepare_candidate(&f.stage, read.input(), Limits::default()).unwrap(),
        CandidateOutcome::Conflicted(_)
    ));
    f.db.close_native_candidate_read(read).await.unwrap();
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_native_ref_updates WHERE kind='land'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    drop(raw);
    drop(other);
    drop(f.db);
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; cancellation, retry and tenant fencing"]
async fn native_cancel_fences_preparation_and_other_owner_cannot_cancel_or_enqueue() {
    let mut f = Fixture::new().await;
    let actors = controls(&mut f).await;
    let other = actor(
        &mut f,
        "other-agent",
        PrincipalKind::Agent,
        &[P::Read, P::QueueRequest],
    )
    .await;
    let other = auth(&mut f, &other, P::QueueRequest).await;
    let entry = proposal(&mut f, &actors, "a", b"a").await;
    let (id, worker) = start(&mut f, &actors, entry).await;
    assert!(matches!(
        f.db.cancel_native_entry(&other, &cmd("cancel-other"), entry)
            .await,
        Err(MetaError::Forbidden)
    ));
    let raw = connection(&f.schema).await;
    let change: Uuid = raw
        .query_one(
            "SELECT changeset FROM th_native_queue WHERE entry=$1",
            &[&entry],
        )
        .await
        .unwrap()
        .get(0);
    assert!(matches!(
        f.db.enqueue_native_change(
            &other,
            &cmd("enqueue-other"),
            Uuid::new_v4(),
            "main",
            change,
            1
        )
        .await,
        Err(MetaError::Forbidden)
    ));
    assert!(
        f.db.native_preparation_replay(&worker, &cmd("prepare"), id)
            .await
            .unwrap()
            .is_none()
    );
    let read = f.db.open_native_candidate(&worker, id).await.unwrap();
    let owner = auth(&mut f, &actors.owner, P::QueueRequest).await;
    f.db.cancel_native_entry(&owner, &cmd("cancel"), entry)
        .await
        .unwrap();
    assert!(matches!(
        f.db.read_native_candidate(&read, 0, &f.depot, &[read.roots()[0].clone()], 1_000_000)
            .await,
        Err(MetaError::IntegrationState)
    ));
    let object = PreparedObject::new(&Object::Blob(b"late".to_vec())).unwrap();
    assert!(matches!(
        f.db.reserve_native_candidate_object(&worker, id, &f.depot, &object)
            .await,
        Err(MetaError::IntegrationState)
    ));
    assert!(matches!(
        f.db.finish_native_preparation_failure(
            &worker,
            &cmd("prepare"),
            id,
            "merge_conflict",
            true,
            json!({})
        )
        .await,
        Err(MetaError::IntegrationState)
    ));
    f.db.close_native_candidate_read(read).await.unwrap();
    let next = Uuid::new_v4();
    f.db.enqueue_native_change(&owner, &cmd("enqueue-again"), next, "main", change, 1)
        .await
        .unwrap();
    let (next_id, worker) = start(&mut f, &actors, next).await;
    let rejected =
        f.db.finish_native_preparation_failure(
            &worker,
            &cmd("reject"),
            next_id,
            "merge_unsupported",
            false,
            json!({"count":1}),
        )
        .await
        .unwrap();
    assert_eq!(
        rejected,
        f.db.native_preparation_replay(&worker, &cmd("reject"), next_id)
            .await
            .unwrap()
            .unwrap()
    );
    let foreign = Scope::new(f.scope.tenant(), Uuid::new_v4()).unwrap();
    f.db.create_repository(&foreign, Authority::Standalone)
        .await
        .unwrap();
    f.db.set_repository_access(
        &foreign,
        "integrator",
        &[P::Read, P::IntegrationControl],
        "operator",
    )
    .await
    .unwrap();
    let token =
        f.db.issue_repository_token(
            &foreign,
            "integrator",
            &[P::Read, P::IntegrationControl],
            3600,
            "operator",
        )
        .await
        .unwrap();
    let foreign =
        f.db.authorize(&token.secret, &foreign, P::Read)
            .await
            .unwrap();
    assert!(matches!(
        f.db.native_candidate_status(&foreign, id).await,
        Err(MetaError::NotFound)
    ));
    drop(raw);
    drop(f.db);
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; populated schema-9 upgrade"]
async fn native_migration_preserves_existing_changes_and_can_enqueue_them() {
    use sha2::{Digest, Sha256};
    let schema = format!("th_test_{}", Uuid::new_v4().simple());
    connection("public")
        .await
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    let raw = connection(&schema).await;
    let mut digest = Vec::new();
    for sql in [
        include_str!("../../migrations/001_transactions.sql"),
        include_str!("../../migrations/002_object_catalog.sql"),
        include_str!("../../migrations/003_change_intake.sql"),
        include_str!("../../migrations/004_access_control.sql"),
        include_str!("../../migrations/005_source_profiles.sql"),
        include_str!("../../migrations/006_source_uploads.sql"),
        include_str!("../../migrations/007_workspace_preparations.sql"),
        include_str!("../../migrations/008_managed_runs.sql"),
        include_str!("../../migrations/009_change_uploads.sql"),
    ] {
        raw.batch_execute(sql).await.unwrap();
        let mut h = Sha256::new();
        h.update(digest);
        h.update(sql.as_bytes());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(9,$1)",
        &[&digest],
    )
    .await
    .unwrap();
    let scope = Scope::new("upgrade", Uuid::new_v4()).unwrap();
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    db.create_repository(&scope, Authority::Standalone)
        .await
        .unwrap();
    let mut f = Fixture::from_db(schema, scope, db).await;
    let prepared = f.prepare(b"old accepted change").await;
    let change = Uuid::new_v4();
    legacy_change(&mut f, "before", change, &prepared).await;
    let snapshot = "SELECT jsonb_build_object('changes',(SELECT jsonb_agg(to_jsonb(v) ORDER BY version) FROM th_change_versions v),'pins',(SELECT jsonb_agg(to_jsonb(p) ORDER BY holder) FROM th_object_pins p),'runs',(SELECT jsonb_agg(to_jsonb(r)-'origin' ORDER BY run) FROM th_agent_runs r),'receipts',(SELECT jsonb_agg(to_jsonb(o) ORDER BY key) FROM th_operations o))";
    let before: Value = raw.query_one(snapshot, &[]).await.unwrap().get(0);
    assert!(matches!(
        f.db.check_schema().await,
        Err(MetaError::SchemaMismatch)
    ));
    f.db.migrate().await.unwrap();
    f.db.migrate().await.unwrap();
    f.db.check_schema().await.unwrap();
    assert_eq!(
        before,
        raw.query_one(snapshot, &[])
            .await
            .unwrap()
            .get::<_, Value>(0)
    );
    let actors = controls(&mut f).await;
    let owner = auth(&mut f, &actors.owner, P::QueueRequest).await;
    let entry = Uuid::new_v4();
    f.db.enqueue_native_change(&owner, &cmd("after"), entry, "main", change, 1)
        .await
        .unwrap();
    let (id, worker) = start(&mut f, &actors, entry).await;
    prepare(&mut f, &worker, id).await;
    drop(raw);
    drop(f.db);
    cleanup(&f.schema).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; global disable racing native publication"]
async fn native_publication_holds_evidence_principal_until_commit() {
    let mut f = Fixture::new().await;
    let actors = controls(&mut f).await;
    let entry = proposal(&mut f, &actors, "race", b"reviewed").await;
    let (id, worker) = start(&mut f, &actors, entry).await;
    prepare(&mut f, &worker, id).await;
    evidence(&mut f, &actors, &worker, id).await;
    let raw = connection(&f.schema).await;
    let lock = (Uuid::new_v4().as_u128() & 0x7fff_ffff_ffff_ffff) as i64;
    raw.query_one("SELECT pg_advisory_lock($1)", &[&lock])
        .await
        .unwrap();
    raw.batch_execute(&format!("CREATE FUNCTION hold_land() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({lock}::bigint); RETURN NEW; END $$; CREATE TRIGGER hold_land BEFORE UPDATE ON th_native_refs FOR EACH ROW EXECUTE FUNCTION hold_land()")).await.unwrap();
    let land_connection = connection(&f.schema).await;
    let land_pid: i32 = land_connection
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let mut land_db = Metadata::new(land_connection).await.unwrap();
    let land = tokio::spawn(async move {
        land_db
            .land_native_candidate(&worker, &cmd("race-land"), id)
            .await
    });
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let waiting:bool=raw.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE pid=$1 AND locktype='advisory' AND NOT granted)",&[&land_pid]).await.unwrap().get(0);
        if waiting {
            break;
        }
        assert!(std::time::Instant::now() < until);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let disable_connection = connection(&f.schema).await;
    let disable_pid: i32 = disable_connection
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let mut disable_db = Metadata::new(disable_connection).await.unwrap();
    let tenant = f.scope.tenant().to_owned();
    let disable = tokio::spawn(async move {
        disable_db
            .disable_principal(&tenant, "reviewer", "operator")
            .await
    });
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let blockers: Vec<i32> = raw
            .query_one("SELECT pg_blocking_pids($1)", &[&disable_pid])
            .await
            .unwrap()
            .get(0);
        if blockers.contains(&land_pid) {
            break;
        }
        assert!(
            !disable.is_finished(),
            "disable returned while an admitted publication still uses that reviewer"
        );
        assert!(std::time::Instant::now() < until);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    raw.query_one("SELECT pg_advisory_unlock($1)", &[&lock])
        .await
        .unwrap();
    assert_eq!(land.await.unwrap().unwrap()["state"], "landed");
    disable.await.unwrap().unwrap();
    assert!(
        !raw.query_one(
            "SELECT enabled FROM th_principals WHERE principal='reviewer'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, bool>(0)
    );
    drop(raw);
    drop(f.db);
    cleanup(&f.schema).await;
}

#[path = "integration/execution.rs"]
mod execution;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn previous_merge_profile_cannot_recompute_pending_candidate_after_upgrade() {
    let mut f = Fixture::new().await;
    let actors = controls(&mut f).await;
    let entry = proposal(&mut f, &actors, "old-profile", b"transition").await;
    let candidate = Uuid::new_v4();
    let worker = auth(&mut f, &actors.worker, P::IntegrationControl).await;
    // Seed the reservation the old binary would have inserted, preserving the
    // immutable identity trigger. Never modify an existing candidate's profile.
    let mut raw = connection(&f.schema).await;
    let tx = raw.transaction().await.unwrap();
    tx.execute("INSERT INTO th_native_candidates(tenant,repository,candidate,entry,expected_head,expected_snapshot,expected_generation,epoch,policy,merge_profile,timestamp_seconds,state,expires_at) SELECT r.tenant,r.repository,$1,q.entry,r.head,$2,r.generation,r.epoch,r.policy,$3,1,'preparing',clock_timestamp()+interval '1 hour' FROM th_native_queue q JOIN th_native_refs r ON r.tenant=q.tenant AND r.repository=q.repository AND r.name=q.target WHERE q.entry=$4",
        &[&candidate, &f.base.as_bytes().as_slice(), &"treeharbor-native-merge-v1/diffy-0.5.2/exact-sibling-renames", &entry]).await.unwrap();
    tx.execute(
        "UPDATE th_native_queue SET state='preparing',candidate=$1 WHERE entry=$2",
        &[&candidate, &entry],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        f.db.open_native_candidate(&worker, candidate).await,
        Err(MetaError::IntegrationState)
    ));
    let owner = auth(&mut f, &actors.owner, P::QueueRequest).await;
    f.db.cancel_native_entry(&owner, &cmd("cancel-old-profile"), entry)
        .await
        .unwrap();
    let entry = proposal(&mut f, &actors, "new-profile", b"transition").await;
    let (candidate, worker) = start(&mut f, &actors, entry).await;
    let prepared = prepare(&mut f, &worker, candidate).await;
    assert_eq!(prepared.profile(), treeharbor_merge::PROFILE);
    assert_ne!(
        prepared.profile(),
        "treeharbor-native-merge-v1/diffy-0.5.2/exact-sibling-renames"
    );
    cleanup(&f.schema).await;
}
