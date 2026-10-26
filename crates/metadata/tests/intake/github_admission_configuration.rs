//! Real database authorization, immutable pin and queue fencing. Provider data is fixture evidence.
use super::*;
fn policy() -> AdmissionPolicy {
    let v: Value = serde_json::from_str(include_str!(
        "../fixtures/connected-admission-policy-v2.json"
    ))
    .unwrap();
    serde_json::from_value(v["policy"].clone()).unwrap()
}
#[tokio::test]
#[ignore = "requires PostgreSQL; reviewed configuration exact binding, CAS, replay and queue ABA fencing"]
async fn reviewed_configuration_is_bound_and_rotation_fences_existing_queue_even_with_same_checks()
{
    let mut f = QueueFixture::with_merge_queue(true).await;
    let p = policy();
    let owner = actor(
        &mut f.p.f,
        "configuration-owner",
        K::Human,
        &[P::PolicyWrite, P::Read],
    )
    .await;
    let reader = actor(&mut f.p.f, "configuration-reader", K::Human, &[P::Read]).await;
    for actor in [&f.owner, &f.bridge] {
        assert!(
            f.p.f
                .db
                .set_github_admission_policy(
                    actor,
                    &cmd("unprivileged-config"),
                    f.p.binding,
                    "main",
                    0,
                    Some(&p)
                )
                .await
                .is_err()
        );
    }
    for case in ["repository", "branch", "checks"] {
        let mut bad = p.clone();
        match case {
            "repository" => {
                let c = bad.host_configuration.as_mut().unwrap();
                c.profile.repository_id = 12;
                c.visibility.target.repository_id = 12;
                c.visibility.legacy.as_mut().unwrap().repository_id = 12;
            }
            "branch" => {
                let c = bad.host_configuration.as_mut().unwrap();
                c.profile.base_ref = "release".into();
                c.visibility.target.base_ref = "release".into();
            }
            _ => bad.workflows[0].required.app_id = 8,
        }
        assert!(
            f.p.f
                .db
                .set_github_admission_policy(&owner, &cmd(case), f.p.binding, "main", 0, Some(&bad))
                .await
                .is_err(),
            "{case}"
        );
    }
    let raw = connection(&f.p.f.schema).await;
    let before: i64 = raw
        .query_one("SELECT count(*) FROM th_operations", &[])
        .await
        .unwrap()
        .get(0);
    let preview =
        f.p.f
            .db
            .plan_github_admission_policy(&owner, 1, f.p.binding, "main", 0, &p)
            .await
            .unwrap();
    assert_eq!(preview["approved"], false);
    assert_eq!(preview["generation"], 1);
    let count: i64 = raw
        .query_one("SELECT count(*) FROM th_operations", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(before, count);
    assert_eq!(preview["host_configuration"]["required_matches"], true);
    let mut next_requirements = p.clone();
    next_requirements.workflows[0].required.name = "CI / next".into();
    let planned =
        f.p.f
            .db
            .plan_github_admission_policy(&owner, 1, f.p.binding, "main", 0, &next_requirements)
            .await
            .unwrap();
    assert_eq!(planned["approved"], false);
    assert_eq!(planned["host_configuration"]["required_matches"], false);
    assert_eq!(planned["required"][0]["name"], "CI / next");
    assert!(
        f.p.f
            .db
            .set_github_admission_policy(
                &owner,
                &cmd("unconfigured-context"),
                f.p.binding,
                "main",
                0,
                Some(&next_requirements)
            )
            .await
            .is_err()
    );
    let first = set(&mut f.p, &owner, 0, Some(&p)).await;
    let read =
        f.p.f
            .db
            .github_admission_policy(&reader, 1, f.p.binding, "main")
            .await
            .unwrap();
    assert_eq!(read["definition"], json!(p));
    assert_eq!(first["policy"], p.digest().unwrap().to_string());
    // A previous PR inspection has different rules and cannot enter this policy.
    f.spec.admission = Some(pin(&first));
    assert!(
        f.p.f
            .db
            .request_github_queue(&f.owner, &cmd("old-rules"), f.id, &f.spec)
            .await
            .is_err()
    );
    let pull: Uuid = raw
        .query_one(
            "SELECT pull FROM th_github_inspections WHERE inspection=$1",
            &[&f.inspection],
        )
        .await
        .unwrap()
        .get(0);
    let inspection = Uuid::new_v4();
    let v =
        f.p.f
            .db
            .request_github_inspection(
                &f.p.owner,
                &cmd("configuration-inspection"),
                inspection,
                &treeharbor_metadata::github::InspectionSpec {
                    pull: pull.to_string(),
                    required: p
                        .required(7, 1, f.p.binding, "main", 1)
                        .unwrap()
                        .into_iter()
                        .collect(),
                    ttl_seconds: 900,
                },
            )
            .await
            .unwrap();
    let mut observation = inspections::report(&f.p, &v).await;
    observation.rules_before = p.rules.clone();
    observation.rules_after = p.rules.clone();
    f.p.f
        .db
        .report_github_inspection(
            &f.p.reader,
            &cmd("configuration-inspection-report"),
            inspection,
            &observation,
        )
        .await
        .unwrap();
    f.spec.inspection = inspection.to_string();
    f.inspection = inspection;
    f.target = QueueTarget {
        pull: serde_json::from_value(v["target"].clone()).unwrap(),
        expected_base_revision: observation.before.base_revision,
        expected_rules: p.rules.clone(),
    };
    let queued = f.request().await;
    let mut replacement = p.clone();
    replacement
        .host_configuration
        .as_mut()
        .unwrap()
        .visibility
        .legacy
        .as_mut()
        .unwrap()
        .id = "BPR_replaced".into();
    let changed = set(&mut f.p, &owner, 1, Some(&replacement)).await;
    assert_eq!(changed["required"], first["required"]);
    assert_ne!(changed["policy"], first["policy"]);
    assert!(
        f.p.f
            .db
            .begin_github_queue(&f.bridge, &cmd("stale-config-queue"), f.id, Uuid::new_v4())
            .await
            .is_err()
    );
    assert_eq!(set(&mut f.p, &owner, 0, Some(&p)).await, first);
    assert_eq!(
        f.p.f
            .db
            .github_admission_policy(&reader, 1, f.p.binding, "main")
            .await
            .unwrap()["generation"],
        2
    );
    set(&mut f.p, &owner, 2, None).await;
    let restored = set(&mut f.p, &owner, 3, Some(&p)).await;
    assert_eq!(restored["policy"], first["policy"]);
    assert_eq!(restored["generation"], 4);
    assert!(
        f.p.f
            .db
            .begin_github_queue(&f.bridge, &cmd("aba-config-queue"), f.id, Uuid::new_v4())
            .await
            .is_err()
    );
    assert_eq!(f.request().await, queued);
    assert!(
        f.p.f
            .db
            .set_github_admission_policy(
                &owner,
                &cmd("stale-config-cas"),
                f.p.binding,
                "main",
                1,
                Some(&p)
            )
            .await
            .is_err()
    );
    let count: i64 = raw
        .query_one("SELECT count(*) FROM th_github_admission_policies", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 2);
    let count: i64 = raw
        .query_one(
            "SELECT count(*) FROM th_github_queue WHERE attempt IS NOT NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 0);
    cleanup(&f.p.f.schema).await;
}
