use super::*;

async fn receipt(f: &QueueFixture, attempt: Uuid) -> QueueObservation {
    let mut r = f.report().await;
    r.version = 3;
    r.enqueue_receipt = Some(EnqueueReceipt {
        request_id: attempt.to_string(),
        repository_id: f.target.pull.repository_id,
        pull_node: f.target.pull.pull_node.clone(),
        pull_number: f.target.pull.pull_number,
        queue_id: "MQ_fixture".into(),
        entry_id: "MQE_fixture".into(),
        head_revision: f.target.pull.head_revision.clone(),
    });
    r
}

#[tokio::test]
#[ignore = "requires PostgreSQL; exact dispatch receipt, replay, late observation retention and explicit dequeue"]
async fn graphql_receipt_retains_identity_without_claiming_current_membership() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let a = f.begin().await;
    let r = receipt(&f, a).await;
    let c = cmd("graphql-receipt");
    let v =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &c, f.id, a, &r)
            .await
            .unwrap();
    assert_eq!(v["state"], "host_enqueued");
    assert_eq!(v["retained_enqueue"], json!(r.enqueue_receipt));
    assert_eq!(v["merge_verified"], false);
    assert!(v["provider_request"].is_null());
    assert_eq!(
        v,
        f.p.f
            .db
            .report_github_queue(&f.bridge, &c, f.id, a, &r)
            .await
            .unwrap()
    );
    let r = f.report().await;
    let newer =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("newer-unknown"), f.id, a, &r)
            .await
            .unwrap();
    assert_eq!(newer["state"], "uncertain");
    assert_eq!(newer["retained_enqueue"], v["retained_enqueue"]);
    assert_eq!(newer["report_sequence"], 2);
    let mut restarted = Metadata::new(connection(&f.p.f.schema).await)
        .await
        .unwrap();
    let observed = restarted
        .github_queue_context(&f.bridge, 1, f.id, true, None)
        .await
        .unwrap();
    assert_eq!(observed["retained_enqueue"], v["retained_enqueue"]);
    let id = Uuid::new_v4();
    let spec = treeharbor_metadata::github::DequeueSpec {
        queue: f.id.to_string(),
        ttl_seconds: 60,
        scope: "pull_request".into(),
    };
    let d = restarted
        .request_github_dequeue(&f.owner, &cmd("remove"), id, &spec)
        .await
        .unwrap();
    assert_eq!(d["enqueue_settled"], true);
    assert_eq!(
        restarted
            .begin_github_dequeue(&f.bridge, &cmd("remove-begin"), id, Uuid::new_v4())
            .await
            .unwrap()["dispatch_granted"],
        true
    );
    assert!(
        restarted
            .request_github_queue(
                &f.owner,
                &cmd("overlapping-enqueue"),
                Uuid::new_v4(),
                &f.spec
            )
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; receipts cannot cross attempts/transports or contradict positive no-send"]
async fn graphql_receipts_reject_conflicting_durable_histories_in_both_orders() {
    for order in [
        "wrong-attempt",
        "different-entry",
        "rest-first",
        "graphql-first",
        "not-sent-first",
        "not-sent-after",
    ] {
        let mut f = QueueFixture::new().await;
        f.request().await;
        let a = f.begin().await;
        let graph = receipt(&f, a).await;
        let mut rest = f.report().await;
        rest.receipt = Some(pending_receipt(&f, Uuid::new_v4()));
        let mut no_send = f.report().await;
        no_send.version = 3;
        no_send.mutation_not_sent = true;
        let (first, mut second) = match order {
            "rest-first" => (rest, graph),
            "graphql-first" => (graph, rest),
            "not-sent-first" => (no_send, graph),
            "not-sent-after" => (graph, no_send),
            _ => (graph.clone(), graph),
        };
        if order == "wrong-attempt" {
            second.enqueue_receipt.as_mut().unwrap().request_id = Uuid::new_v4().to_string();
        }
        if order == "different-entry" {
            second.enqueue_receipt.as_mut().unwrap().entry_id = "MQE_different".into();
        }
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("first"), f.id, a, &first)
            .await
            .unwrap();
        assert!(
            f.p.f
                .db
                .report_github_queue(&f.bridge, &cmd("second"), f.id, a, &second)
                .await
                .is_err(),
            "{order}"
        );
        cleanup(&f.p.f.schema).await;
    }
}
