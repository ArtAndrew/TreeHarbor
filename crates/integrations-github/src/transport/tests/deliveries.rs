use super::*;
use crate::{DeliveryAttempt, RecoveryTarget};
use uuid::Uuid;

const CALLBACK: &str = "https://hooks.example.com/github/events";
const GUID: &str = "0b989ba4-242f-11e5-81e1-c7b6966d2516";
const PRIVATE_MARKER: &str = "private-provider-payload-not-for-output";
fn target() -> RecoveryTarget {
    RecoveryTarget {
        app_id: 7,
        binding: binding(),
        callback_url: CALLBACK.into(),
    }
}
fn app() -> Value {
    json!({"id":7,"client_id":"Iv1_public_test"})
}
fn config() -> Value {
    json!({"url":CALLBACK,"content_type":"json","insecure_ssl":"0","secret":PRIVATE_MARKER})
}
fn preflight() -> Vec<Vec<u8>> {
    vec![
        response(200, app()),
        response(200, installation()),
        response(200, config()),
    ]
}
fn attempt() -> Value {
    let time = time::OffsetDateTime::from_unix_timestamp(now().unwrap() as i64 - 60)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    json!({"id":12345678,"guid":GUID,"delivered_at":time,"redelivery":false,
        "duration":0.27,"status":"Service Unavailable","status_code":503,
        "event":"merge_group","action":"checks_requested","installation_id":9,
        "repository_id":11,"throttled_at":null})
}
fn expected(value: &Value) -> DeliveryAttempt {
    DeliveryAttempt {
        id: value["id"].as_u64().unwrap(),
        guid: Uuid::parse_str(value["guid"].as_str().unwrap()).unwrap(),
        installation_id: 9,
        repository_id: 11,
        delivered_at_unix_ms: time::OffsetDateTime::parse(
            value["delivered_at"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap()
        .unix_timestamp()
            * 1000,
        redelivery: value["redelivery"].as_bool().unwrap(),
        action: "checks_requested".into(),
        status_code: Some(503),
        throttled_at_unix_ms: None,
    }
}
fn detail(value: &Value) -> Value {
    let mut d = value.clone();
    d["url"] = json!(CALLBACK);
    d["request"] = json!({"headers":{"Authorization":PRIVATE_MARKER,"X-Hub-Signature-256":PRIVATE_MARKER},"payload":{"untrusted":PRIVATE_MARKER}});
    d["response"] = json!({"payload":PRIVATE_MARKER});
    d
}
fn page(body: Value, link: &str) -> Vec<u8> {
    raw(
        200,
        &format!("Content-Type: application/json\r\nLink: {link}\r\n"),
        &serde_json::to_vec(&body).unwrap(),
    )
}
fn assert_jwts(requests: &[Vec<u8>]) {
    for bytes in requests {
        let request = request_text(bytes);
        let token = request
            .lines()
            .find_map(|line| line.strip_prefix("authorization: Bearer "))
            .unwrap();
        let pieces: Vec<_> = token.split('.').collect();
        assert_eq!(pieces.len(), 3);
        let claims: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(pieces[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "Iv1_public_test");
        assert!(!request.contains(TEST_TOKEN));
        assert!(!request.contains("access_tokens"));
    }
}

#[tokio::test]
async fn app_jwt_page_includes_pending_and_success_and_filters_other_scopes() {
    let a = attempt();
    let mut pending = a.clone();
    pending["id"] = json!(12345679);
    pending["status_code"] = Value::Null;
    pending["redelivery"] = json!(true);
    let mut success = a.clone();
    success["id"] = json!(12345680);
    success["status_code"] = json!(302);
    let mut other = a.clone();
    other["id"] = json!(8);
    other["repository_id"] = json!(12);
    let mut other_install = a.clone();
    other_install["id"] = json!(9);
    other_install["installation_id"] = json!(10);
    let mut other_event = a.clone();
    other_event["id"] = json!(10);
    other_event["event"] = json!("push");
    let mut replies = preflight();
    replies.push(page(json!([a,pending,success,other,other_install,other_event]),
        "<https://api.github.com/app/hook/deliveries?per_page=100&cursor=bmV4dA%3D%3D>; rel=\"next\""));
    let (connection, server) = host(replies).await;
    let result = connection
        .list_webhook_deliveries(&target(), None)
        .await
        .unwrap();
    assert_eq!(result.scanned, 6);
    assert_eq!(result.attempts.len(), 3);
    assert_eq!(result.attempts[0], expected(&a));
    assert_eq!(result.attempts[1].status_code, None);
    assert_eq!(result.attempts[2].status_code, Some(302));
    assert!(result.attempts.iter().all(|a| a.guid.to_string() == GUID));
    assert_eq!(result.next_cursor.as_deref(), Some("bmV4dA=="));
    let encoded = serde_json::to_string(&result).unwrap();
    assert!(!encoded.contains(PRIVATE_MARKER));
    let requests = server.await.unwrap();
    assert_jwts(&requests);
    assert!(request_text(&requests[3]).starts_with("GET /app/hook/deliveries?per_page=100 "));
    assert!(!request_text(&requests[3]).contains("status="));
    let mut replies = preflight();
    replies.push(response(200, json!([])));
    let (connection, server) = host(replies).await;
    let result = connection
        .list_webhook_deliveries(&target(), Some("bmV4dA=="))
        .await
        .unwrap();
    assert_eq!(result.cursor.as_deref(), Some("bmV4dA=="));
    assert!(result.next_cursor.is_none());
    assert!(
        request_text(&server.await.unwrap()[3])
            .starts_with("GET /app/hook/deliveries?per_page=100&cursor=bmV4dA%3D%3D ")
    );
}

#[tokio::test]
async fn refuses_cross_app_owner_and_insecure_or_changed_callback_before_listing() {
    let mut cases = Vec::new();
    for (field, v) in [("id", json!(8)), ("client_id", json!("another_client"))] {
        let mut a = app();
        a[field] = v;
        cases.push(vec![response(200, a)]);
    }
    for (field, v) in [
        ("id", json!(10)),
        ("app_id", json!(8)),
        (
            "account",
            json!({"id":14,"login":"example-org","type":"Organization"}),
        ),
        ("suspended_at", json!("2026-01-01T00:00:00Z")),
    ] {
        let mut i = installation();
        i[field] = v;
        cases.push(vec![response(200, app()), response(200, i)]);
    }
    for (field, v) in [
        ("url", json!("https://elsewhere.example/github/events")),
        ("content_type", json!("form")),
        ("insecure_ssl", json!(1)),
        ("insecure_ssl", json!(true)),
    ] {
        let mut c = config();
        c[field] = v;
        cases.push(vec![
            response(200, app()),
            response(200, installation()),
            response(200, c),
        ]);
    }
    for responses in cases {
        let count = responses.len();
        let (connection, server) = host(responses).await;
        let error = connection
            .list_webhook_deliveries(&target(), None)
            .await
            .unwrap_err();
        assert!(!format!("{error:?} {error}").contains(PRIVATE_MARKER));
        assert_eq!(server.await.unwrap().len(), count);
    }
}

#[tokio::test]
async fn malicious_ambiguous_and_looping_links_are_not_followed_or_returned() {
    let links = [
        "<https://evil.example/app/hook/deliveries?per_page=100&cursor=x>; rel=\"next\"",
        "<https://api.github.com:444/app/hook/deliveries?per_page=100&cursor=x>; rel=\"next\"",
        "<http://api.github.com/app/hook/deliveries?per_page=100&cursor=x>; rel=\"next\"",
        "<https://secret@api.github.com/app/hook/deliveries?per_page=100&cursor=x>; rel=\"next\"",
        "<https://api.github.com/other?per_page=100&cursor=x>; rel=\"next\"",
        "<https://api.github.com/app/hook/deliveries?per_page=100&cursor=x#private>; rel=\"next\"",
        "<https://api.github.com/app/hook/deliveries?per_page=100&cursor=x&cursor=y>; rel=\"next\"",
        "<https://api.github.com/app/hook/deliveries?per_page=100&cursor=x&status=failure>; rel=\"next\"",
        "<https://api.github.com/app/hook/deliveries?per_page=100&cursor=x>; rel=\"next\", <https://api.github.com/app/hook/deliveries?per_page=100&cursor=y>; rel=\"next\"",
        "<https://api.github.com/app/hook/deliveries?per_page=100&cursor=current>; rel=\"next\"",
        "<https://api.github.com/app/hook/deliveries?per_page=100&cursor=x>; rel=\"unknown\"",
        "<https://api.github.com/app/hook/deliveries?per_page=100&cursor=x>; rel=\"next\"; title=\"extra\"",
        "<https://api.github.com/app/hook/deliveries?per_page=30&cursor=x>; rel=\"next\"",
        "<https://api.github.com/app/hook/deliveries?per_page=100>; rel=\"next\"",
    ];
    for link in links {
        let mut replies = preflight();
        replies.push(page(json!([]), link));
        let (connection, server) = host(replies).await;
        assert_eq!(
            connection
                .list_webhook_deliveries(&target(), Some("current"))
                .await
                .unwrap_err(),
            Error::WebhookPagination
        );
        assert_eq!(server.await.unwrap().len(), 4);
    }
}

#[tokio::test]
async fn consumed_duplicates_missing_fields_and_delivery_bounds_fail_closed() {
    let a = attempt();
    let mut bodies = Vec::new();
    for (field, v) in [
        ("id", json!(0)),
        ("id", json!(u64::MAX)),
        ("guid", json!(Uuid::nil().to_string())),
        ("guid", json!("not-a-guid")),
        ("status_code", json!(600)),
        ("status_code", json!(-1)),
        ("delivered_at", json!("2099-01-01T00:00:00Z")),
        ("throttled_at", json!("2099-01-01T00:00:00Z")),
        ("action", json!("unknown-action")),
        ("repository_id", json!("11")),
    ] {
        let mut row = a.clone();
        row[field] = v;
        bodies.push(json!([row]).to_string());
    }
    for field in [
        "status_code",
        "installation_id",
        "repository_id",
        "throttled_at",
    ] {
        let mut row = a.clone();
        row.as_object_mut().unwrap().remove(field);
        bodies.push(json!([row]).to_string());
    }
    bodies.push(format!(
        "[{}]",
        a.to_string().replacen('{', "{\"id\":12345678,", 1)
    ));
    bodies.push(json!([a.clone(), a.clone()]).to_string());
    let too_many: Vec<_> = (0..101)
        .map(|id| {
            let mut v = a.clone();
            v["id"] = json!(id + 1);
            v
        })
        .collect();
    bodies.push(json!(too_many).to_string());
    for body in bodies {
        let mut replies = preflight();
        replies.push(raw(
            200,
            "Content-Type: application/json\r\n",
            body.as_bytes(),
        ));
        let (connection, server) = host(replies).await;
        assert!(
            connection
                .list_webhook_deliveries(&target(), None)
                .await
                .is_err()
        );
        assert_eq!(server.await.unwrap().len(), 4);
    }
}

#[tokio::test]
async fn redelivery_checks_original_attempt_then_posts_once_without_claiming_callback() {
    let a = attempt();
    let expected = expected(&a);
    let mut replies = preflight();
    replies.push(response(200, detail(&a)));
    replies.push(raw(202, "", b""));
    let (connection, server) = host(replies).await;
    let receipt = connection
        .redeliver_webhook(&target(), &expected, || async { Ok(()) })
        .await
        .unwrap();
    assert_eq!(receipt.attempt, expected);
    assert!(!receipt.callback_confirmed);
    let output = serde_json::to_string(&receipt).unwrap();
    assert!(!output.contains(PRIVATE_MARKER));
    assert!(!output.contains("signature"));
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 5);
    assert_jwts(&requests);
    assert!(request_text(&requests[3]).starts_with("GET /app/hook/deliveries/12345678 "));
    assert!(request_text(&requests[4]).starts_with("POST /app/hook/deliveries/12345678/attempts "));
    assert!(!request_text(&requests[4]).contains(PRIVATE_MARKER));
}

#[tokio::test]
async fn mismatched_detail_cannot_redeliver_another_repository_or_guid() {
    let a = attempt();
    let expected = expected(&a);
    for (field, v) in [
        ("id", json!(12345679)),
        ("guid", json!(Uuid::new_v4().to_string())),
        ("url", json!("https://elsewhere.example/github/events")),
        ("installation_id", json!(10)),
        ("repository_id", json!(12)),
        ("event", json!("push")),
        ("action", json!("destroyed")),
        ("redelivery", json!(true)),
        ("delivered_at", json!("2020-01-01T00:00:00Z")),
    ] {
        let mut d = detail(&a);
        d[field] = v;
        let mut replies = preflight();
        replies.push(response(200, d));
        let (connection, server) = host(replies).await;
        let error = connection
            .redeliver_webhook(&target(), &expected, || async {
                panic!("must not authorize substituted attempt")
            })
            .await
            .unwrap_err();
        assert_eq!(error.error, Error::WebhookRecovery);
        assert!(error.mutation_not_sent);
        assert_eq!(server.await.unwrap().len(), 4);
    }
}

#[tokio::test]
async fn final_revocation_blocks_post_and_lost_response_never_retries_automatically() {
    let a = attempt();
    let expected = expected(&a);
    let mut replies = preflight();
    replies.push(response(200, detail(&a)));
    let (connection, server) = host(replies).await;
    let error = connection
        .redeliver_webhook(&target(), &expected, || async {
            Err(Error::PublicationStopped)
        })
        .await
        .unwrap_err();
    assert!(error.mutation_not_sent);
    assert_eq!(error.error, Error::PublicationStopped);
    assert_eq!(server.await.unwrap().len(), 4);
    for reply in [
        Vec::new(),
        response(503, json!({"message":PRIVATE_MARKER})),
        raw(302, "Location: https://evil.example/\r\n", b""),
        raw(429, "Retry-After: 120\r\n", PRIVATE_MARKER.as_bytes()),
        response(202, json!({"unexpected":PRIVATE_MARKER})),
    ] {
        let mut replies = preflight();
        replies.push(response(200, detail(&a)));
        replies.push(reply);
        let (connection, server) = host(replies).await;
        let error = connection
            .redeliver_webhook(&target(), &expected, || async { Ok(()) })
            .await
            .unwrap_err();
        assert!(!error.mutation_not_sent);
        assert!(!format!("{error:?}").contains(PRIVATE_MARKER));
        assert_eq!(connection.slots.available_permits(), 4);
        assert_eq!(server.await.unwrap().len(), 5);
    }
}

#[tokio::test]
async fn local_invalid_targets_cursors_and_expired_attempts_make_no_requests() {
    let (connection, server) = host(vec![]).await;
    for callback in [
        "http://hooks.example.com/github/events",
        "https://hooks.example.com/control",
        "https://hooks.example.com/github/events?token=private",
        "https://user@hooks.example.com/github/events",
        "https://hooks.example.com/github/events#frag",
    ] {
        let mut t = target();
        t.callback_url = callback.into();
        assert!(connection.list_webhook_deliveries(&t, None).await.is_err());
    }
    for cursor in ["", "x y", "x\n", "é"] {
        assert!(
            connection
                .list_webhook_deliveries(&target(), Some(cursor))
                .await
                .is_err()
        );
    }
    let mut e = expected(&attempt());
    e.delivered_at_unix_ms -= 4 * 24 * 3600 * 1000;
    let error = connection
        .redeliver_webhook(&target(), &e, || async { panic!("expired attempt") })
        .await
        .unwrap_err();
    assert!(error.mutation_not_sent);
    assert!(server.await.unwrap().is_empty());
}

#[tokio::test]
async fn detail_payload_is_bounded_and_never_becomes_callback_evidence() {
    let a = attempt();
    let expected = expected(&a);
    // A legitimate large parsed payload can exceed the ordinary 128KiB API cap.
    let mut d = detail(&a);
    d["request"]["payload"] = json!("x".repeat(256 * 1024));
    let mut replies = preflight();
    replies.push(response(200, d));
    replies.push(raw(202, "", b""));
    let (connection, server) = host(replies).await;
    let receipt = connection
        .redeliver_webhook(&target(), &expected, || async { Ok(()) })
        .await
        .unwrap();
    assert!(!receipt.callback_confirmed);
    assert!(serde_json::to_vec(&receipt).unwrap().len() < 2048);
    server.await.unwrap();
    let oversized = vec![b' '; 8 * 1024 * 1024 + 1];
    let mut replies = preflight();
    replies.push(raw(200, "Content-Type: application/json\r\n", &oversized));
    let (connection, server) = host(replies).await;
    let error = connection
        .redeliver_webhook(&target(), &expected, || async {
            panic!("oversized detail")
        })
        .await
        .unwrap_err();
    assert!(error.mutation_not_sent);
    assert_eq!(
        error.error,
        Error::Size {
            stage: Stage::WebhookDeliveryDetail
        }
    );
    assert_eq!(server.await.unwrap().len(), 4);
}

#[tokio::test]
async fn opaque_cursor_is_encoded_as_data_instead_of_changing_filters() {
    let mut replies = preflight();
    replies.push(response(200, json!([])));
    let (connection, server) = host(replies).await;
    let cursor = "opaque&status=failure?+/%=";
    connection
        .list_webhook_deliveries(&target(), Some(cursor))
        .await
        .unwrap();
    let requests = server.await.unwrap();
    let path = request_text(&requests[3])
        .split_whitespace()
        .nth(1)
        .unwrap();
    let url = Url::parse(&format!("https://api.github.com{path}")).unwrap();
    let pairs: Vec<_> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("per_page".into(), "100".into()),
            ("cursor".into(), cursor.into())
        ]
    );
}

#[tokio::test]
async fn duplicate_security_fields_in_detail_and_config_cannot_enable_redelivery() {
    let a = attempt();
    let expected = expected(&a);
    for field in ["id", "guid", "repository_id", "installation_id", "url"] {
        let d = detail(&a);
        let duplicate = d
            .to_string()
            .replacen('{', &format!("{{\"{field}\":{},", d[field]), 1);
        let mut replies = preflight();
        replies.push(raw(
            200,
            "Content-Type: application/json\r\n",
            duplicate.as_bytes(),
        ));
        let (connection, server) = host(replies).await;
        let error = connection
            .redeliver_webhook(&target(), &expected, || async {
                panic!("duplicate detail")
            })
            .await
            .unwrap_err();
        assert!(error.mutation_not_sent);
        assert_eq!(
            error.error,
            Error::Response {
                stage: Stage::WebhookDeliveryDetail
            }
        );
        assert_eq!(server.await.unwrap().len(), 4);
    }
    let duplicate = config()
        .to_string()
        .replacen('{', "{\"insecure_ssl\":\"1\",", 1);
    let (connection, server) = host(vec![
        response(200, app()),
        response(200, installation()),
        raw(
            200,
            "Content-Type: application/json\r\n",
            duplicate.as_bytes(),
        ),
    ])
    .await;
    assert!(
        connection
            .list_webhook_deliveries(&target(), None)
            .await
            .is_err()
    );
    assert_eq!(server.await.unwrap().len(), 3);
}

#[tokio::test]
async fn cancellation_after_actual_post_never_replays_and_releases_capacity() {
    let a = attempt();
    let expected = expected(&a);
    let mut replies = preflight();
    replies.push(response(200, detail(&a)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        for response in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            while !received.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                received.extend_from_slice(&buf[..n]);
                assert!(received.len() < 16384);
            }
            socket.write_all(&response).await.unwrap();
        }
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        while !received.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0);
            received.extend_from_slice(&buf[..n]);
            assert!(received.len() < 16384);
        }
        assert!(
            request_text(&received).starts_with("POST /app/hook/deliveries/12345678/attempts ")
        );
        seen_tx.send(()).unwrap();
        finish_rx.await.unwrap();
        drop(socket);
        assert!(
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        );
    });
    let connection = Arc::new(Connection {
        client: client(false).unwrap(),
        base,
        credentials: credentials(),
        slots: Arc::new(Semaphore::new(1)),
    });
    let worker = tokio::spawn({
        let connection = connection.clone();
        async move {
            connection
                .redeliver_webhook(&target(), &expected, || async { Ok(()) })
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), seen_rx)
        .await
        .unwrap()
        .unwrap();
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    assert_eq!(connection.slots.available_permits(), 1);
    finish_tx.send(()).unwrap();
    server.await.unwrap();
    // No receipt exists for the killed invocation. The future durable intent
    // ledger must retain an unknown outcome rather than declare it unsent.
}

#[tokio::test]
async fn documented_dotted_client_id_is_preserved_in_jwt_and_checked_against_app() {
    let client_id = "Iv1.ab1112223334445c";
    let mut a = app();
    a["client_id"] = json!(client_id);
    let (mut connection, server) = host(vec![
        response(200, a),
        response(200, installation()),
        response(200, config()),
        response(200, json!([])),
    ])
    .await;
    connection.credentials = AppCredentials::from_pem(7, client_id.into(), PEM.to_vec()).unwrap();
    connection
        .list_webhook_deliveries(&target(), None)
        .await
        .unwrap();
    for bytes in server.await.unwrap() {
        let request = request_text(&bytes);
        let token = request
            .lines()
            .find_map(|s| s.strip_prefix("authorization: Bearer "))
            .unwrap();
        let claims: Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(token.split('.').nth(1).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(claims["iss"], client_id);
    }
}
