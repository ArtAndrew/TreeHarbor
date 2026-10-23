use super::*;
use crate::BranchOutcome;
use std::os::unix::fs::PermissionsExt;
use treeharbor_core::{Changes, Commit, Directory, Object, ObjectStore, RevisionRef};
use treeharbor_git_bridge::publish;
use treeharbor_storage::{LocalStore, TenantId};
use uuid::Uuid;

fn prepared() -> (tempfile::TempDir, publish::Prepared) {
    prepared_with_timeout(Duration::from_secs(10))
}
fn prepared_with_timeout(timeout: Duration) -> (tempfile::TempDir, publish::Prepared) {
    let d = tempfile::tempdir().unwrap();
    fs::set_permissions(d.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let store = LocalStore::open(d.path(), &TenantId::new("publisher-fixture").unwrap()).unwrap();
    let dir = store
        .put(&Object::Directory(Directory::new(vec![]).unwrap()))
        .unwrap();
    let snapshot = store.put(&Object::Snapshot { root: dir }).unwrap();
    let changes = store
        .put(&Object::ChangeOperations(Changes::new(vec![]).unwrap()))
        .unwrap();
    let native = store
        .put(&Object::NativeCommit(Commit {
            parents: vec![],
            snapshot,
            changes,
            author_name: "Test".into(),
            author_email: "test@example.invalid".into(),
            timestamp_seconds: 1,
            timezone_minutes: 0,
            message: "fixture".into(),
            git_origin: None,
        }))
        .unwrap();
    let p = publish::prepare(
        &store,
        std::path::Path::new("/usr/bin/git"),
        d.path(),
        Uuid::from_bytes([7; 16]),
        native,
        treeharbor_git_bridge::Limits {
            timeout,
            ..Default::default()
        },
    )
    .unwrap();
    (d, p)
}
fn write_token() -> Value {
    let mut v = token();
    v["permissions"]["contents"] = json!("write");
    v
}
fn access(value: Value) -> Vec<Vec<u8>> {
    vec![
        response(200, installation()),
        response(201, value),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, repository()),
    ]
}
fn reference(branch: &str, sha: &str) -> Value {
    json!({"ref":branch,"object":{"type":"commit","sha":sha}})
}
fn oid(p: &publish::Prepared) -> String {
    RevisionRef::Git(p.git_oid())
        .to_string()
        .trim_start_matches("git:sha1:")
        .to_owned()
}

#[tokio::test]
async fn publication_guard_checks_before_mint_and_again_before_push() {
    for stop_at in [1, 2] {
        let (_d, p) = prepared();
        let responses = if stop_at == 1 {
            vec![]
        } else {
            let mut responses = access(write_token());
            responses.push(response(404, json!({"message":"missing"})));
            responses.push(revoked());
            responses
        };
        let (c, server) = host(responses).await;
        let mut calls = 0;
        let outcome = c
            .publish_branch_guarded(&binding(), "/absent-worker".into(), p, None, || {
                calls += 1;
                std::future::ready(if calls == stop_at {
                    Err(Error::PublicationStopped)
                } else {
                    Ok(())
                })
            })
            .await;
        assert_eq!(outcome.unwrap_err(), Error::PublicationStopped);
        assert_eq!(calls, stop_at);
        assert_eq!(
            server.await.unwrap().len(),
            if stop_at == 1 { 0 } else { 6 }
        );
        assert_eq!(c.slots.available_permits(), 4);
    }
}

#[tokio::test]
async fn publisher_scopes_write_token_and_reconciles_an_already_present_exact_commit() {
    let (_d, p) = prepared();
    let expected = oid(&p);
    let branch = p.branch().to_owned();
    let mut responses = access(write_token());
    responses.push(response(200, reference(&branch, &expected)));
    responses.push(revoked());
    let (c, server) = host(responses).await;
    // The nonexistent worker proves that reconciliation does not repush.
    let result = c
        .publish_branch(&binding(), "/absent-worker".into(), p, None)
        .await
        .unwrap();
    assert_eq!(result.outcome, BranchOutcome::AlreadyPresent);
    assert!(result.token_revoked);
    let wire = serde_json::to_string(&result).unwrap();
    assert!(!wire.contains(TEST_TOKEN));
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 6);
    let mint: Value =
        serde_json::from_str(request_text(&requests[1]).split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(
        mint,
        json!({"repository_ids":[11],"permissions":{"contents":"write","metadata":"read"}})
    );
    assert_eq!(c.slots.available_permits(), 4);
}

#[tokio::test]
async fn publisher_rejects_readonly_or_excess_scope_and_always_attempts_revocation() {
    for permissions in [
        json!({"contents":"read","metadata":"read"}),
        json!({"contents":"write","metadata":"read","workflows":"write"}),
        json!({"contents":"write","metadata":"read","administration":"write"}),
    ] {
        let (_d, p) = prepared();
        let mut t = write_token();
        t["permissions"] = permissions;
        let (c, server) = host(vec![
            response(200, installation()),
            response(201, t),
            revoked(),
        ])
        .await;
        assert_eq!(
            c.publish_branch(&binding(), "/absent-worker".into(), p, None)
                .await
                .unwrap_err(),
            Error::TokenScope
        );
        assert_eq!(server.await.unwrap().len(), 3);
        assert_eq!(c.slots.available_permits(), 4);
    }
}

#[tokio::test]
async fn publisher_never_overwrites_an_unexpected_branch_or_writes_an_archived_repo() {
    for archived in [false, true] {
        let (_d, p) = prepared();
        let mut responses = access(write_token());
        if archived {
            let mut repo = repository();
            repo["archived"] = json!(true);
            responses[3] = response(200, repo);
        } else {
            responses.push(response(200, reference(p.branch(), &"11".repeat(20))));
        }
        responses.push(revoked());
        let (c, server) = host(responses).await;
        let error = c
            .publish_branch(&binding(), "/absent-worker".into(), p, None)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            if archived {
                Error::RepositoryUnavailable
            } else {
                Error::RefChanged
            }
        );
        assert!(
            request_text(server.await.unwrap().last().unwrap())
                .starts_with("DELETE /installation/token ")
        );
    }
}

#[tokio::test]
async fn publisher_reconciles_failed_worker_but_keeps_ambiguous_absence_unknown() {
    for found in [false, true] {
        let (_d, p) = prepared();
        let expected = oid(&p);
        let branch = p.branch().to_owned();
        let mut responses = access(write_token());
        responses.push(response(404, json!({"message":"missing"})));
        responses.push(if found {
            response(200, reference(&branch, &expected))
        } else {
            response(404, json!({"message":"missing"}))
        });
        responses.push(revoked());
        let (c, server) = host(responses).await;
        let result = c
            .publish_branch(&binding(), "/absent-worker".into(), p, None)
            .await;
        if found {
            assert_eq!(
                result.unwrap().outcome,
                BranchOutcome::ObservedAfterUncertainPush
            );
        } else {
            assert_eq!(result.unwrap_err(), Error::PublicationUnknown);
        }
        assert_eq!(server.await.unwrap().len(), 7);
        assert_eq!(c.slots.available_permits(), 4);
    }
}

#[tokio::test]
async fn branch_observation_uses_read_access_and_requires_the_exact_ref_identity() {
    for wrong in [false, true] {
        let id = Uuid::from_bytes([7; 16]);
        let branch = publish::branch(id).unwrap();
        let mut responses = access(token());
        responses.push(response(
            200,
            reference(
                if wrong { "refs/heads/main" } else { &branch },
                &"11".repeat(20),
            ),
        ));
        responses.push(revoked());
        let (c, server) = host(responses).await;
        let result = c.observe_branch(&binding(), id).await;
        if wrong {
            assert_eq!(result.unwrap_err(), Error::Identity);
        } else {
            assert_eq!(
                result.unwrap().git_revision,
                Some(format!("git:sha1:{}", "11".repeat(20)))
            );
        }
        let requests = server.await.unwrap();
        let mint = request_text(&requests[1]);
        assert!(mint.contains("\"contents\":\"read\""));
        assert!(!mint.contains("\"write\""));
    }
}

#[tokio::test]
async fn failed_revocation_preserves_the_observed_head_and_never_repeats_a_push() {
    let (_d, p) = prepared();
    let expected = oid(&p);
    let branch = p.branch().to_owned();
    let mut responses = access(write_token());
    responses.push(response(404, json!({"message":"missing"})));
    responses.push(response(200, reference(&branch, &expected)));
    responses.push(response(503, json!({"message":"unavailable"})));
    let (c, server) = host(responses).await;
    let result = c
        .publish_branch(&binding(), "/absent-worker".into(), p, None)
        .await
        .unwrap();
    assert_eq!(result.outcome, BranchOutcome::ObservedAfterUncertainPush);
    assert_eq!(result.git_revision, format!("git:sha1:{expected}"));
    assert!(!result.token_revoked);
    assert_eq!(server.await.unwrap().len(), 7);

    let mut responses = access(token());
    responses.push(response(200, reference(&branch, &expected)));
    responses.push(response(503, json!({"message":"unavailable"})));
    let (c, server) = host(responses).await;
    let result = c
        .observe_branch(&binding(), Uuid::from_bytes([7; 16]))
        .await
        .unwrap();
    assert_eq!(result.git_revision, Some(format!("git:sha1:{expected}")));
    assert!(!result.token_revoked);
    assert_eq!(server.await.unwrap().len(), 6);
}

#[tokio::test]
async fn post_push_bad_or_unavailable_observation_remains_unknown_and_is_not_retried() {
    for reply in [
        response(503, json!({"message":"unavailable"})),
        response(200, reference("refs/heads/main", &"11".repeat(20))),
        response(
            200,
            json!({"ref":"refs/heads/treeharbor/07070707070707070707070707070707","object":{"type":"tree","sha":"11".repeat(20)}}),
        ),
        Vec::new(),
    ] {
        let (_d, p) = prepared();
        let mut responses = access(write_token());
        responses.push(response(404, json!({"message":"missing"})));
        responses.push(reply);
        responses.push(revoked());
        let (c, server) = host(responses).await;
        assert_eq!(
            c.publish_branch(&binding(), "/absent-worker".into(), p, None)
                .await
                .unwrap_err(),
            Error::PublicationUnknown
        );
        assert_eq!(server.await.unwrap().len(), 7);
        assert_eq!(c.slots.available_permits(), 4);
    }
}

#[tokio::test]
async fn publisher_requires_token_lifetime_for_the_whole_bounded_push() {
    let (_d, p) = prepared_with_timeout(Duration::from_secs(120));
    let mut t = write_token();
    t["expires_at"] = json!(
        time::OffsetDateTime::from_unix_timestamp(now().unwrap() as i64 + 120)
            .unwrap()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    );
    let mut responses = access(t);
    responses.push(response(404, json!({"message":"missing"})));
    responses.push(revoked());
    let (c, server) = host(responses).await;
    assert_eq!(
        c.publish_branch(&binding(), "/absent-worker".into(), p, None)
            .await
            .unwrap_err(),
        Error::TokenLifetime
    );
    assert_eq!(server.await.unwrap().len(), 6);
}
