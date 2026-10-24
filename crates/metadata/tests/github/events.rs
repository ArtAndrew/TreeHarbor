use super::*;
use serde_json::Value;
use treeharbor_github_webhook::{VerifiedDelivery, WebhookKey};
use treeharbor_metadata::github::EventChannelSpec;
fn verified(name: &str, app: u64) -> VerifiedDelivery {
    let data: Value = serde_json::from_str(include_str!(
        "../../../github-webhook/tests/fixtures/vectors-v1.json"
    ))
    .unwrap();
    let v = data["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap();
    WebhookKey::new(
        app,
        Uuid::from_bytes([1; 16]),
        data["secret"].as_str().unwrap().as_bytes().to_vec(),
    )
    .unwrap()
    .authenticate(
        v["event_header"].as_str().unwrap(),
        v["signature"].as_str().unwrap(),
        v["body"].as_str().unwrap().as_bytes().to_vec(),
    )
    .unwrap()
}
async fn ready(f: &mut Fixture) -> Uuid {
    f.bind().await;
    let probe = Uuid::new_v4();
    let a = f.begin(probe).await;
    f.db.finish_github_probe(&a, &cmd("observed"), f.binding, probe, &observation())
        .await
        .unwrap();
    let channel = Uuid::new_v4();
    let a = f.owner().await;
    f.db.create_github_event_channel(
        &a,
        &cmd("channel"),
        channel,
        &EventChannelSpec {
            binding: f.binding.to_string(),
            max_events: 2,
            max_bytes: 4096,
            max_aliases: 2,
        },
    )
    .await
    .unwrap();
    channel
}
async fn counts(f: &Fixture) -> (i64, i64, i64) {
    let c = connection(&f.schema).await;
    let r=c.query_one("SELECT (SELECT count(*) FROM th_github_events),(SELECT count(*) FROM th_github_event_deliveries),(SELECT count(*) FROM th_outbox WHERE kind='github.events.received')",&[]).await.unwrap();
    (r.get(0), r.get(1), r.get(2))
}
#[tokio::test]
#[ignore = "requires PostgreSQL: signed inbox dedup, immutable bytes and lost-response recovery"]
async fn signed_events_replay_with_original_bytes_and_one_outbox_fact() {
    let mut f = Fixture::new().await;
    let channel = ready(&mut f).await;
    let delivery = Uuid::new_v4();
    let v = verified("checks", 7);
    let receipt = f.db.accept_github_event(delivery, &v).await.unwrap();
    f.db = Metadata::new(connection(&f.schema).await).await.unwrap();
    assert_eq!(
        receipt,
        f.db.accept_github_event(delivery, &v).await.unwrap()
    );
    assert_eq!(
        receipt,
        f.db.accept_github_event(Uuid::new_v4(), &v).await.unwrap()
    );
    assert_eq!(counts(&f).await, (1, 2, 1));
    assert!(matches!(
        f.db.accept_github_event(Uuid::new_v4(), &v).await,
        Err(Error::HostEventLimit)
    ));
    assert!(matches!(
        f.db.accept_github_event(delivery, &verified("destroyed-merged", 7))
            .await,
        Err(Error::IdempotencyConflict)
    ));
    assert_eq!(counts(&f).await, (1, 2, 1));
    let raw = connection(&f.schema).await;
    let r = raw
        .query_one("SELECT body,digest,normalized FROM th_github_events", &[])
        .await
        .unwrap();
    assert_eq!(r.get::<_, Vec<u8>>(0), v.body());
    assert_eq!(r.get::<_, Vec<u8>>(1), v.digest());
    assert_eq!(
        r.get::<_, Value>(2),
        serde_json::from_str::<Value>(include_str!(
            "../../../github-webhook/tests/fixtures/checks-normalized.json"
        ))
        .unwrap()
    );
    assert!(
        raw.execute("UPDATE th_github_events SET body=decode('00','hex')", &[])
            .await
            .is_err()
    );
    assert!(
        raw.execute("DELETE FROM th_github_event_deliveries", &[])
            .await
            .is_err()
    );
    let a = f.reader().await;
    let page = f.db.github_event_page(&a, 1, channel, 0).await.unwrap();
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    assert_eq!(page["merge_verified"], false);
    assert_eq!(page["channel"]["retained_bytes"], v.body().len());
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL: independent creation/destruction facts and capacity backpressure"]
async fn reverse_order_does_not_erase_destruction_and_capacity_never_drops_accepted_events() {
    let mut f = Fixture::new().await;
    let channel = ready(&mut f).await;
    for name in ["destroyed-invalidated", "checks"] {
        f.db.accept_github_event(Uuid::new_v4(), &verified(name, 7))
            .await
            .unwrap();
    }
    assert!(matches!(
        f.db.accept_github_event(Uuid::new_v4(), &verified("destroyed-merged", 7))
            .await,
        Err(Error::HostEventLimit)
    ));
    assert_eq!(counts(&f).await, (2, 2, 2));
    let a = f.reader().await;
    let page = f.db.github_event_page(&a, 1, channel, 0).await.unwrap();
    assert_eq!(page["events"][0]["fact"]["reason"], "invalidated");
    assert_eq!(page["events"][1]["fact"]["action"], "checks_requested");
    assert_eq!(page["merge_verified"], false);
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL: authorization, provider identity, channel revocation and authority fences"]
async fn channels_require_observed_binding_and_fence_foreign_app_revoked_binding_and_epoch() {
    let mut f = Fixture::new().await;
    f.bind().await;
    let a = f.owner().await;
    assert!(matches!(
        f.db.create_github_event_channel(
            &a,
            &cmd("premature"),
            Uuid::new_v4(),
            &EventChannelSpec {
                binding: f.binding.to_string(),
                max_events: 1,
                max_bytes: 4096,
                max_aliases: 1
            }
        )
        .await,
        Err(Error::HostBindingState)
    ));
    let probe = Uuid::new_v4();
    let a = f.begin(probe).await;
    f.db.finish_github_probe(&a, &cmd("observed"), f.binding, probe, &observation())
        .await
        .unwrap();
    let id = Uuid::new_v4();
    let a = f.owner().await;
    let spec = EventChannelSpec {
        binding: f.binding.to_string(),
        max_events: 1,
        max_bytes: 4096,
        max_aliases: 1,
    };
    let result =
        f.db.create_github_event_channel(&a, &cmd("channel"), id, &spec)
            .await
            .unwrap();
    assert_eq!(
        result,
        f.db.create_github_event_channel(&a, &cmd("channel"), id, &spec)
            .await
            .unwrap()
    );
    assert!(matches!(
        f.db.accept_github_event(Uuid::new_v4(), &verified("checks", 999))
            .await,
        Err(Error::HostEventState)
    ));
    let raw = connection(&f.schema).await;
    raw.execute(
        "UPDATE th_repositories SET epoch=2 WHERE tenant=$1 AND repository=$2",
        &[&f.scope.tenant(), &f.scope.repository()],
    )
    .await
    .unwrap();
    assert!(matches!(
        f.db.accept_github_event(Uuid::new_v4(), &verified("checks", 7))
            .await,
        Err(Error::Fenced)
    ));
    raw.execute(
        "UPDATE th_repositories SET epoch=1 WHERE tenant=$1 AND repository=$2",
        &[&f.scope.tenant(), &f.scope.repository()],
    )
    .await
    .unwrap();
    let a = f.owner().await;
    f.db.revoke_github(&a, &cmd("revoke-binding"), f.binding)
        .await
        .unwrap();
    assert!(matches!(
        f.db.accept_github_event(Uuid::new_v4(), &verified("checks", 7))
            .await,
        Err(Error::HostBindingState)
    ));
    f.db.revoke_github_event_channel(&a, &cmd("revoke-channel"), id)
        .await
        .unwrap();
    assert!(matches!(
        f.db.accept_github_event(Uuid::new_v4(), &verified("checks", 7))
            .await,
        Err(Error::HostEventState)
    ));
    assert_eq!(counts(&f).await, (0, 0, 0));
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL: concurrent duplicate arrival and transaction rollback"]
async fn concurrent_delivery_and_outbox_failure_leave_one_atomic_fact() {
    let mut f = Fixture::new().await;
    ready(&mut f).await;
    let raw = connection(&f.schema).await;
    raw.batch_execute("CREATE FUNCTION reject_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='github.events.received' THEN RAISE EXCEPTION 'fixture rollback'; END IF; RETURN NEW; END; $$; CREATE TRIGGER reject_event BEFORE INSERT ON th_outbox FOR EACH ROW EXECUTE FUNCTION reject_event()").await.unwrap();
    let id = Uuid::new_v4();
    assert!(
        f.db.accept_github_event(id, &verified("checks", 7))
            .await
            .is_err()
    );
    assert_eq!(counts(&f).await, (0, 0, 0));
    let n: i64 = raw
        .query_one("SELECT retained_events FROM th_github_event_channels", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0);
    raw.batch_execute("DROP TRIGGER reject_event ON th_outbox; DROP FUNCTION reject_event()")
        .await
        .unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let schema = f.schema.clone();
        tasks.spawn(async move {
            let mut db = Metadata::new(connection(&schema).await).await.unwrap();
            db.accept_github_event(id, &verified("checks", 7))
                .await
                .unwrap()
        });
    }
    let first = tasks.join_next().await.unwrap().unwrap();
    while let Some(r) = tasks.join_next().await {
        assert_eq!(first, r.unwrap());
    }
    assert_eq!(counts(&f).await, (1, 1, 1));
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL: populated schema22 upgrade without implicit event subscriptions"]
async fn populated_schema22_upgrade_preserves_bindings_and_requires_explicit_channel() {
    use sha2::{Digest, Sha256};
    let mut f = Fixture::new().await;
    f.bind().await;
    let probe = Uuid::new_v4();
    let a = f.begin(probe).await;
    f.db.finish_github_probe(&a, &cmd("observed"), f.binding, probe, &observation())
        .await
        .unwrap();
    let raw = connection(&f.schema).await;
    raw.batch_execute("DROP TABLE th_github_event_deliveries; DROP TABLE th_github_events; DROP TABLE th_github_event_channels; DROP FUNCTION th_github_event_channel_immutable()").await.unwrap();
    let mut files: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .collect();
    files.sort();
    let mut digest = Vec::new();
    for path in files.into_iter().take(22) {
        let mut h = Sha256::new();
        h.update(digest);
        h.update(std::fs::read(path).unwrap());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=22,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    let snapshot = "SELECT jsonb_build_object('binding',(SELECT jsonb_agg(to_jsonb(b)) FROM th_github_bindings b),'probe',(SELECT jsonb_agg(to_jsonb(p)) FROM th_github_probes p),'access',(SELECT jsonb_agg(to_jsonb(a)) FROM th_repository_access a),'tokens',(SELECT jsonb_agg(to_jsonb(t)) FROM th_access_tokens t))";
    let before: Value = raw.query_one(snapshot, &[]).await.unwrap().get(0);
    f.db.migrate().await.unwrap();
    f.db.migrate().await.unwrap();
    f.db.check_schema().await.unwrap();
    let after: Value = raw.query_one(snapshot, &[]).await.unwrap().get(0);
    assert_eq!(before, after);
    assert!(matches!(
        f.db.accept_github_event(Uuid::new_v4(), &verified("checks", 7))
            .await,
        Err(Error::HostEventState)
    ));
    let a = f.owner().await;
    f.db.create_github_event_channel(
        &a,
        &cmd("explicit"),
        Uuid::new_v4(),
        &EventChannelSpec {
            binding: f.binding.to_string(),
            max_events: 1,
            max_bytes: 4096,
            max_aliases: 1,
        },
    )
    .await
    .unwrap();
    f.db.accept_github_event(Uuid::new_v4(), &verified("checks", 7))
        .await
        .unwrap();
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL: tenant routing uniqueness, read isolation and provider identity"]
async fn event_routes_cannot_be_claimed_by_a_second_tenant_or_selected_by_headers() {
    let mut f = Fixture::new().await;
    let channel = ready(&mut f).await;
    for name in [
        "foreign-installation",
        "foreign-repository",
        "foreign-owner",
    ] {
        assert!(matches!(
            f.db.accept_github_event(Uuid::new_v4(), &verified(name, 7))
                .await,
            Err(Error::HostEventState)
        ));
    }
    let mut db = Metadata::new(connection(&f.schema).await).await.unwrap();
    let scope = Scope::new("other-tenant", Uuid::new_v4()).unwrap();
    db.create_repository(&scope, Authority::Connected)
        .await
        .unwrap();
    let human = token(
        &mut db,
        &scope,
        "owner",
        K::Human,
        &[P::Read, P::RepositoryWrite],
    )
    .await;
    let bridge = token(
        &mut db,
        &scope,
        "bridge",
        K::Bridge,
        &[P::Read, P::HostRead],
    )
    .await;
    let mut other = Fixture {
        schema: f.schema.clone(),
        scope,
        db,
        human,
        bridge,
        binding: Uuid::new_v4(),
        spec: spec(),
    };
    other.bind().await;
    let probe = Uuid::new_v4();
    let a = other.begin(probe).await;
    other
        .db
        .finish_github_probe(&a, &cmd("observed"), other.binding, probe, &observation())
        .await
        .unwrap();
    let a = other.owner().await;
    assert!(matches!(
        other
            .db
            .create_github_event_channel(
                &a,
                &cmd("claim"),
                Uuid::new_v4(),
                &EventChannelSpec {
                    binding: other.binding.to_string(),
                    max_events: 1,
                    max_bytes: 4096,
                    max_aliases: 1
                }
            )
            .await,
        Err(Error::HostEventState)
    ));
    let a = other.reader().await;
    assert!(matches!(
        other.db.github_event_page(&a, 1, channel, 0).await,
        Err(Error::NotFound)
    ));
    let receipt =
        f.db.accept_github_event(Uuid::new_v4(), &verified("checks", 7))
            .await
            .unwrap();
    assert_eq!(receipt.channel, channel);
    let raw = connection(&f.schema).await;
    let tenant: String = raw
        .query_one("SELECT tenant FROM th_github_events", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(tenant, f.scope.tenant());
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL: byte budget atomic rejection and revoked owner credential"]
async fn byte_quota_and_revoked_owner_credentials_do_not_partially_create_state() {
    let mut f = Fixture::new().await;
    f.bind().await;
    let probe = Uuid::new_v4();
    let a = f.begin(probe).await;
    f.db.finish_github_probe(&a, &cmd("observed"), f.binding, probe, &observation())
        .await
        .unwrap();
    let a = f.owner().await;
    let id = Uuid::new_v4();
    f.db.create_github_event_channel(
        &a,
        &cmd("tiny"),
        id,
        &EventChannelSpec {
            binding: f.binding.to_string(),
            max_events: 4,
            max_bytes: 1,
            max_aliases: 1,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        f.db.accept_github_event(Uuid::new_v4(), &verified("checks", 7))
            .await,
        Err(Error::HostEventLimit)
    ));
    assert_eq!(counts(&f).await, (0, 0, 0));
    f.db.set_repository_access(&f.scope, "owner", &[P::Read], "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.revoke_github_event_channel(&a, &cmd("revoked-owner"), id)
            .await,
        Err(Error::Forbidden)
    ));
    cleanup(&f.schema).await;
}
