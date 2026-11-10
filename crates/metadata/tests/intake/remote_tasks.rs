use super::*;
use treeharbor_metadata::{
    proposals::ChangeUploadSpec, remote_tasks::RemoteTaskSpec, uploads::UploadLimits,
};

fn id(value: &Value, key: &str) -> Uuid {
    value[key].as_str().unwrap().parse().unwrap()
}

async fn dispatcher(f: &mut Fixture) -> (Authorization, Authorization) {
    let name = "dispatcher";
    let permissions = [P::Read, P::RepositoryWrite];
    f.db.provision_principal(f.scope.tenant(), name, PrincipalKind::Human, "operator")
        .await
        .unwrap();
    f.db.set_repository_access(&f.scope, name, &permissions, "operator")
        .await
        .unwrap();
    let token =
        f.db.issue_repository_token(&f.scope, name, &permissions, 600, "operator")
            .await
            .unwrap();
    (
        f.db.authorize(&token.secret, &f.scope, P::RepositoryWrite)
            .await
            .unwrap(),
        f.db.authorize(&token.secret, &f.scope, P::Read)
            .await
            .unwrap(),
    )
}

#[tokio::test]
#[ignore = "requires PostgreSQL and object store; two workers race, reclaim and accept atomically"]
async fn remote_task_claim_reclaim_and_change_are_fenced_by_one_live_attempt() {
    let mut f = Fixture::new().await;
    let (dispatch, inspect) = dispatcher(&mut f).await;
    let a = owner(&mut f, "worker-a").await;
    let b = owner(&mut f, "worker-b").await;
    let unrelated = owner(&mut f, "unrelated").await;
    let mut spec = RemoteTaskSpec {
        task: Uuid::new_v4(),
        base: f.revision,
        profile: SourceProfile::full().id(),
        agent: "controller/1".into(),
        label: "edit-file".into(),
        workers: vec!["worker-b".into(), "worker-a".into()],
    };
    let task = Some(spec.task);
    assert!(matches!(
        f.db.enqueue_remote_task(&ctx(&a.0, "no-enqueue"), &spec)
            .await,
        Err(MetaError::Forbidden)
    ));
    let queued =
        f.db.enqueue_remote_task(&ctx(&dispatch, "enqueue"), &spec)
            .await
            .unwrap();
    spec.workers.reverse();
    assert_eq!(
        f.db.enqueue_remote_task(&ctx(&dispatch, "enqueue"), &spec)
            .await
            .unwrap(),
        queued
    );
    assert_eq!(
        f.db.claim_remote_task(&ctx(&unrelated.0, "not-listed"), task, 60)
            .await
            .unwrap(),
        json!({"state":"empty"})
    );
    assert!(matches!(
        f.db.remote_task_status(&unrelated.1, spec.task).await,
        Err(MetaError::NotFound)
    ));

    let raw = connection(&f.schema).await;
    let mut other = Metadata::new(connection(&f.schema).await).await.unwrap();
    let a_claim = ctx(&a.0, "claim-a");
    let b_claim = ctx(&b.0, "claim-b");
    let (first_a, first_b) = tokio::join!(
        f.db.claim_remote_task(&a_claim, task, 60),
        other.claim_remote_task(&b_claim, task, 60)
    );
    let (first_a, first_b) = (first_a.unwrap(), first_b.unwrap());
    let (old, new, old_claim, new_claim, lease, empty) = if first_a["state"] == "leased" {
        (&a, &b, &a_claim, &b_claim, first_a, first_b)
    } else {
        (&b, &a, &b_claim, &a_claim, first_b, first_a)
    };
    assert_eq!(empty, json!({"state":"empty"}));
    assert_eq!(lease["attempt"], 1);
    assert_eq!(lease["base"], f.revision.to_string());
    assert_eq!(
        f.db.claim_remote_task(old_claim, task, 60).await.unwrap(),
        lease
    );
    assert_eq!(
        f.db.remote_task_status(&inspect, spec.task).await.unwrap()["lease_active"],
        true
    );
    assert!(
        f.db.remote_task_status(&new.1, spec.task)
            .await
            .unwrap()
            .get("workspace")
            .is_none()
    );
    assert!(matches!(
        f.db.renew_remote_task(&ctx(&new.0, "steal-renew"), spec.task, 1, 60)
            .await,
        Err(MetaError::StaleLease)
    ));
    let renew = ctx(&old.0, "renew");
    let renewed =
        f.db.renew_remote_task(&renew, spec.task, 1, 90)
            .await
            .unwrap();
    assert!(
        renewed["lease_expires_at_unix_ms"].as_i64().unwrap()
            > lease["lease_expires_at_unix_ms"].as_i64().unwrap()
    );
    assert_eq!(
        f.db.renew_remote_task(&renew, spec.task, 1, 90)
            .await
            .unwrap(),
        renewed
    );
    let old_run = id(&lease, "run");
    let old_workspace = id(&lease, "workspace");
    assert!(matches!(
        f.db.close_api_run(&ctx(&old.0, "bypass-close"), old_run, 1)
            .await,
        Err(MetaError::ExecutionState)
    ));
    let fence = SourceFence {
        workspace: old_workspace,
        generation: 1,
        authority: Authority::Standalone,
        epoch: 1,
    };
    let read = f.db.open_workspace_source(&old.1, fence, 60).await.unwrap();
    let upload = ChangeUploadSpec {
        upload: Uuid::new_v4(),
        run: old_run,
        generation: 1,
        limits: UploadLimits {
            objects: 5,
            bytes: 4096,
            ttl_seconds: 60,
        },
    };
    f.db.begin_change_upload(&ctx(&old.2, "old-upload"), "local-1", &upload)
        .await
        .unwrap();

    raw.execute("UPDATE th_remote_tasks SET lease_until=clock_timestamp()-interval '1 second' WHERE task=$1", &[&spec.task]).await.unwrap();
    assert!(
        !f.db.remote_task_status(&inspect, spec.task).await.unwrap()["lease_active"]
            .as_bool()
            .unwrap()
    );
    assert!(matches!(
        f.db.finish_workspace_source(read).await,
        Err(MetaError::StaleLease)
    ));
    let blob = PreparedObject::new(&Object::Blob(b"old attempt".to_vec())).unwrap();
    assert!(matches!(
        f.db.reserve_change_blob(&old.2, upload.upload, &f.depot, &blob)
            .await,
        Err(MetaError::StaleLease)
    ));
    assert!(matches!(
        f.db.renew_remote_task(&ctx(&old.0, "late-renew"), spec.task, 1, 60)
            .await,
        Err(MetaError::StaleLease)
    ));
    assert_eq!(
        f.db.claim_remote_task(&ctx(&old.0, "old-claim-again"), task, 60)
            .await
            .unwrap(),
        json!({"state":"empty"})
    );

    let replacement = other.claim_remote_task(new_claim, task, 60).await.unwrap();
    assert_eq!(replacement["attempt"], 2);
    assert_ne!(replacement["run"], lease["run"]);
    assert_eq!(
        f.db.api_run_status(&old.1, old_run).await.unwrap()["state"],
        "closed"
    );
    let new_fence = SourceFence {
        workspace: id(&replacement, "workspace"),
        ..fence
    };
    assert!(matches!(
        f.db.open_workspace_source(&old.1, new_fence, 60).await,
        Err(MetaError::NotFound)
    ));
    let read =
        f.db.open_workspace_source(&new.1, new_fence, 60)
            .await
            .unwrap();
    f.db.finish_workspace_source(read).await.unwrap();

    let (applied, operations, result) = f.prepare(b"current attempt").await;
    let submission = ChangeSubmission {
        id: Uuid::new_v4(),
        previous_version: 0,
        run: id(&replacement, "run"),
        workspace_generation: 1,
        applied: &applied,
        operations: &operations,
        result: &result,
    };
    let accept = ctx(&new.2, "remote-accept");
    raw.batch_execute(
        r#"CREATE FUNCTION reject_remote_receipt() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN RAISE EXCEPTION 'test-only lost transaction'; END $$;
        CREATE TRIGGER reject_remote_receipt BEFORE INSERT ON th_operations FOR EACH ROW
        WHEN (NEW.key='remote-accept') EXECUTE FUNCTION reject_remote_receipt();"#,
    )
    .await
    .unwrap();
    assert!(f.db.accept_change(&accept, &submission).await.is_err());
    assert_eq!(
        f.db.remote_task_status(&inspect, spec.task).await.unwrap()["state"],
        "leased"
    );
    assert!(matches!(
        f.db.change_version(&f.scope, submission.id, 1).await,
        Err(MetaError::NotFound)
    ));
    raw.batch_execute(
        "DROP TRIGGER reject_remote_receipt ON th_operations; DROP FUNCTION reject_remote_receipt",
    )
    .await
    .unwrap();

    let accepted = f.db.accept_change(&accept, &submission).await.unwrap();
    assert_eq!(
        accepted["remote_task"],
        json!({"task":spec.task,"attempt":2})
    );
    let recovered = other.accept_change(&accept, &submission).await.unwrap();
    assert_eq!(recovered, accepted);
    assert_eq!(
        f.db.change_version(&f.scope, submission.id, 1)
            .await
            .unwrap(),
        accepted
    );
    let status = other.remote_task_status(&inspect, spec.task).await.unwrap();
    assert_eq!(status["state"], "accepted");
    assert_eq!(status["changeset"], submission.id.to_string());
    assert_eq!(status["lease_active"], false);
    assert_eq!(
        f.db.api_run_status(&new.1, submission.run).await.unwrap()["state"],
        "closed"
    );
    assert!(matches!(
        other
            .cancel_remote_task(&ctx(&dispatch, "cancel-accepted"), spec.task)
            .await,
        Err(MetaError::ExecutionState)
    ));
    assert_eq!(
        other
            .claim_remote_task(&ctx(&new.0, "claim-accepted"), task, 60)
            .await
            .unwrap(),
        json!({"state":"empty"})
    );
    raw.execute(
        "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second'",
        &[],
    )
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
            "SELECT count(*) FROM th_object_pins WHERE holder IN ($1,$2)",
            &[
                &format!("workspace:{old_workspace}"),
                &format!("workspace:{}", new_fence.workspace)
            ]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );

    spec.task = Uuid::new_v4();
    f.db.enqueue_remote_task(&ctx(&dispatch, "enqueue-cancel"), &spec)
        .await
        .unwrap();
    let live =
        f.db.claim_remote_task(&ctx(&a.0, "claim-cancel"), None, 60)
            .await
            .unwrap();
    assert_eq!(id(&live, "task"), spec.task);
    let cancel = ctx(&dispatch, "cancel");
    let cancelled = other.cancel_remote_task(&cancel, spec.task).await.unwrap();
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(
        f.db.cancel_remote_task(&cancel, spec.task).await.unwrap(),
        cancelled
    );
    assert_eq!(
        f.db.api_run_status(&a.1, id(&live, "run")).await.unwrap()["state"],
        "closed"
    );
    cleanup(&f.schema).await;
}
