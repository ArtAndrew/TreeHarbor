use super::*;
#[path = "tests/check_publication.rs"]
mod check_publication_tests;
#[path = "tests/evidence.rs"]
mod evidence_tests;
#[path = "tests/publisher.rs"]
mod publisher_tests;
#[path = "tests/pulls.rs"]
mod pull_tests;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{fs, process::Command};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const PEM: &[u8] = include_bytes!("../../tests/fixtures/test-app.pem");
const PUBLIC: &[u8] = include_bytes!("../../tests/fixtures/test-app.pub.pem");
const TEST_TOKEN: &str = "ghs_public-test-fixture-never-a-real-token";
fn credentials() -> AppCredentials {
    AppCredentials::from_pem(7, "Iv1_public_test".into(), PEM.to_vec()).unwrap()
}
fn binding() -> RepositoryBinding {
    RepositoryBinding {
        installation_id: 9,
        repository_id: 11,
        owner_id: 13,
        owner: "example-org".into(),
        name: "demo".into(),
    }
}
fn installation() -> Value {
    json!({"id":9,"app_id":7,"account":{"id":13,"login":"example-org","type":"Organization"},"suspended_at":null})
}
fn token() -> Value {
    let expiry = time::OffsetDateTime::from_unix_timestamp(now().unwrap() as i64 + 3500)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    json!({"token":TEST_TOKEN,"expires_at":expiry,"permissions":{"contents":"read","metadata":"read"},"repositories":[{"id":11}]})
}
fn repository() -> Value {
    json!({"id":11,"node_id":"R_test","name":"demo","owner":{"id":13,"login":"example-org","type":"Organization"},"private":false,"default_branch":"main","archived":false,"disabled":false})
}
fn response(status: u16, body: Value) -> Vec<u8> {
    raw(
        status,
        "Content-Type: application/json\r\n",
        &serde_json::to_vec(&body).unwrap(),
    )
}
fn raw(status: u16, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut r = format!(
        "HTTP/1.1 {status} Test\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    r.extend_from_slice(body);
    r
}
fn revoked() -> Vec<u8> {
    raw(204, "", b"")
}
fn good_responses() -> Vec<Vec<u8>> {
    vec![
        response(200, installation()),
        response(201, token()),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, repository()),
        revoked(),
    ]
}
// Actual TCP/HTTP transport fixtures, not evidence of GitHub App installation
// or the real-host PR/merge-queue release gate. Test endpoint is private to cfg(test).
async fn host(responses: Vec<Vec<u8>>) -> (Connection, tokio::task::JoinHandle<Vec<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let handle = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in responses {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                assert!(bytes.len() < 16 * 1024);
                if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                    let size: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .map(|v| v.parse().unwrap())
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + size {
                        break;
                    }
                }
            }
            requests.push(bytes);
            if !response.is_empty() {
                let _ = socket.write_all(&response).await;
            }
        }
        // Detect an automatic replay of a token request or a followed redirect.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
        requests
    });
    (
        Connection {
            client: client(false).unwrap(),
            base,
            credentials: credentials(),
            slots: Arc::new(Semaphore::new(4)),
        },
        handle,
    )
}
fn request_text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

#[tokio::test]
async fn failed_git_download_revokes_its_fresh_scoped_token_and_releases_capacity() {
    let (connection, server) = host(good_responses()).await;
    let request = crate::GitDownload {
        worker: "/missing-treeharbor-worker".into(),
        git: "/missing-git".into(),
        output: "/missing-output".into(),
        commit: treeharbor_core::GitOid::Sha1([7; 20]),
        limits: Default::default(),
    };
    let error = connection
        .download_git(&binding(), request)
        .await
        .unwrap_err();
    assert_eq!(error, Error::GitDownload);
    assert!(!error.to_string().contains(TEST_TOKEN));
    assert_eq!(connection.slots.available_permits(), 4);
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 5);
    assert!(request_text(&requests[4]).starts_with("DELETE /installation/token "));
}

#[tokio::test]
async fn probe_scopes_each_request_pins_identities_and_revokes_without_publication() {
    let (connection, server) = host(good_responses()).await;
    let probe = connection.probe(&binding()).await.unwrap();
    assert!(probe.read_connection_verified && probe.probe_token_revoked);
    assert!(!probe.publication_ready && !probe.queue_configuration_verified);
    assert_eq!(
        probe.queue_eligibility,
        QueueEligibility::PublicOrganizationEligible
    );
    let json = serde_json::to_string(&probe).unwrap();
    assert!(!json.contains(TEST_TOKEN));
    let mut wire: Value = serde_json::from_str(&json).unwrap();
    assert!(wire["observed_at_unix"].as_u64().unwrap() > 42);
    wire["observed_at_unix"] = 42.into();
    let golden: Value =
        serde_json::from_slice(include_bytes!("../../tests/fixtures/probe-v1.json")).unwrap();
    assert_eq!(wire, golden);
    let requests = server.await.unwrap();
    let expected = [
        "GET /app/installations/9 ",
        "POST /app/installations/9/access_tokens ",
        "GET /installation/repositories?per_page=100 ",
        "GET /repos/example-org/demo ",
        "DELETE /installation/token ",
    ];
    for (r, path) in requests.iter().zip(expected) {
        let r = request_text(r);
        assert!(r.starts_with(path));
        assert!(r.contains("x-github-api-version: 2026-03-10\r\n"));
        assert!(r.contains("accept: application/vnd.github+json\r\n"));
    }
    let mint = request_text(&requests[1]);
    let body: Value = serde_json::from_str(mint.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(
        body,
        json!({"repository_ids":[11],"permissions":{"contents":"read","metadata":"read"}})
    );
    assert!(!mint.contains(TEST_TOKEN));
    assert!(request_text(&requests[4]).contains(TEST_TOKEN));
}

#[tokio::test]
async fn refuses_wrong_app_installation_owner_and_suspension_before_minting() {
    for (field, value, error) in [
        ("id", json!(8), Error::Identity),
        ("app_id", json!(6), Error::Identity),
        (
            "account",
            json!({"id":99,"login":"example-org","type":"Organization"}),
            Error::Identity,
        ),
        (
            "suspended_at",
            json!("2026-01-01T00:00:00Z"),
            Error::Suspended,
        ),
    ] {
        let mut install = installation();
        install[field] = value;
        let (connection, server) = host(vec![response(200, install)]).await;
        assert_eq!(connection.probe(&binding()).await.unwrap_err(), error);
        assert_eq!(server.await.unwrap().len(), 1);
    }
    let mut install = installation();
    install.as_object_mut().unwrap().remove("suspended_at");
    let (connection, server) = host(vec![response(200, install)]).await;
    assert_eq!(
        connection.probe(&binding()).await.unwrap_err(),
        Error::Response {
            stage: Stage::Installation
        }
    );
    server.await.unwrap();
}

#[tokio::test]
async fn refuses_broad_tokens_and_expired_tokens_and_attempts_revocation() {
    let mut cases = Vec::new();
    let mut broad = token();
    broad["permissions"]["contents"] = "write".into();
    cases.push(broad);
    let mut broad = token();
    broad["permissions"]["administration"] = "read".into();
    cases.push(broad);
    let mut broad = token();
    broad["repositories"] = json!([{"id":11},{"id":12}]);
    cases.push(broad);
    let mut expired = token();
    expired["expires_at"] = "2000-01-01T00:00:00Z".into();
    cases.push(expired);
    let mut future = token();
    future["expires_at"] = "2099-01-01T00:00:00Z".into();
    cases.push(future);
    for token in cases {
        let (connection, server) = host(vec![
            response(200, installation()),
            response(201, token),
            revoked(),
        ])
        .await;
        assert!(matches!(
            connection.probe(&binding()).await,
            Err(Error::TokenScope | Error::TokenLifetime)
        ));
        assert!(request_text(&server.await.unwrap()[2]).starts_with("DELETE /installation/token "));
    }
}

#[tokio::test]
async fn enumerates_actual_token_scope_and_rejects_repository_replacement() {
    let mut replies = good_responses();
    // Token replies may omit repository listing; independent enumeration is required.
    let mut t = token();
    t.as_object_mut().unwrap().remove("repositories");
    replies[1] = response(201, t);
    let (connection, server) = host(replies).await;
    connection.probe(&binding()).await.unwrap();
    server.await.unwrap();
    for body in [
        json!({"total_count":2,"repositories":[{"id":11}]}),
        json!({"total_count":1,"repositories":[{"id":12}]}),
    ] {
        let (connection, server) = host(vec![
            response(200, installation()),
            response(201, token()),
            response(200, body),
            revoked(),
        ])
        .await;
        assert_eq!(
            connection.probe(&binding()).await.unwrap_err(),
            Error::TokenScope
        );
        server.await.unwrap();
    }
    for (key, value) in [
        ("id", json!(12)),
        (
            "owner",
            json!({"id":14,"login":"example-org","type":"Organization"}),
        ),
        ("name", json!("other")),
    ] {
        let mut replies = good_responses();
        let mut repo = repository();
        repo[key] = value;
        replies[3] = response(200, repo);
        let (connection, server) = host(replies).await;
        assert_eq!(
            connection.probe(&binding()).await.unwrap_err(),
            Error::Identity
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn queue_eligibility_does_not_claim_readiness_or_private_plan_entitlement() {
    for (kind, private, expected) in [
        (
            "User",
            false,
            QueueEligibility::PersonalRepositoryUnsupported,
        ),
        (
            "Organization",
            true,
            QueueEligibility::PrivateOrganizationPlanRequired,
        ),
    ] {
        let mut replies = good_responses();
        let mut install = installation();
        install["account"]["type"] = kind.into();
        let mut repo = repository();
        repo["owner"]["type"] = kind.into();
        repo["private"] = private.into();
        repo["archived"] = true.into();
        replies[0] = response(200, install);
        replies[3] = response(200, repo);
        let (connection, server) = host(replies).await;
        let probe = connection.probe(&binding()).await.unwrap();
        assert_eq!(probe.queue_eligibility, expected);
        assert!(probe.archived);
        assert!(!probe.publication_ready);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn redirects_http_errors_and_lost_mutation_responses_do_not_retry_or_leak() {
    let (connection, server) =
        host(vec![raw(302, "Location: /steal\r\n", b"private-response")]).await;
    assert_eq!(
        connection.probe(&binding()).await.unwrap_err(),
        Error::Http {
            stage: Stage::Installation,
            status: 302,
            retry_after_seconds: None
        }
    );
    server.await.unwrap();
    let (connection, server) = host(vec![raw(
        429,
        "Retry-After: 120\r\n",
        TEST_TOKEN.as_bytes(),
    )])
    .await;
    let error = connection.probe(&binding()).await.unwrap_err();
    assert_eq!(
        error,
        Error::Http {
            stage: Stage::Installation,
            status: 429,
            retry_after_seconds: Some(120)
        }
    );
    assert!(!format!("{error:?} {error}").contains(TEST_TOKEN));
    server.await.unwrap();
    let (connection, server) = host(vec![response(200, installation()), Vec::new()]).await;
    assert_eq!(
        connection.probe(&binding()).await.unwrap_err(),
        Error::Transport {
            stage: Stage::Token
        }
    );
    assert_eq!(server.await.unwrap().len(), 2);
}

#[tokio::test]
async fn malformed_compressed_and_oversized_responses_fail_closed() {
    let oversized = vec![b' '; MAX_RESPONSE + 1];
    let chunked = format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n", oversized.len(),String::from_utf8(oversized.clone()).unwrap()).into_bytes();
    for (reply, error) in [
        (
            raw(200, "Content-Type: text/html\r\n", b"<secret>"),
            Error::Response {
                stage: Stage::Installation,
            },
        ),
        (
            raw(
                200,
                "Content-Type: application/json\r\nContent-Encoding: gzip\r\n",
                b"compressed",
            ),
            Error::Response {
                stage: Stage::Installation,
            },
        ),
        (
            raw(
                200,
                "Content-Type: application/json\r\n",
                b"{malformed-secret}",
            ),
            Error::Response {
                stage: Stage::Installation,
            },
        ),
        (
            raw(200, "Content-Type: application/json\r\n", &oversized),
            Error::Size {
                stage: Stage::Installation,
            },
        ),
        (
            chunked,
            Error::Size {
                stage: Stage::Installation,
            },
        ),
    ] {
        let (connection, server) = host(vec![reply]).await;
        assert_eq!(connection.probe(&binding()).await.unwrap_err(), error);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn revocation_failure_is_not_a_successful_probe() {
    let mut replies = good_responses();
    replies[4] = response(503, json!({"message":TEST_TOKEN}));
    let (connection, server) = host(replies).await;
    assert_eq!(
        connection.probe(&binding()).await.unwrap_err(),
        Error::Http {
            stage: Stage::RevokeToken,
            status: 503,
            retry_after_seconds: None
        }
    );
    server.await.unwrap();
}

#[test]
fn rsa_jwt_has_bounded_claims_and_independent_openssl_signature() {
    let credentials = credentials();
    let jwt = credentials.jwt(1_800_000_000).unwrap().header().unwrap();
    assert!(jwt.is_sensitive());
    let pieces: Vec<_> = jwt
        .to_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap()
        .split('.')
        .collect();
    let header: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(pieces[0]).unwrap()).unwrap();
    let claims: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(pieces[1]).unwrap()).unwrap();
    assert_eq!(header, json!({"typ":"JWT","alg":"RS256"}));
    assert_eq!(
        claims,
        json!({"iat":1_799_999_940_u64,"exp":1_800_000_300_u64,"iss":"Iv1_public_test"})
    );
    let dir = tempfile::tempdir().unwrap();
    let pubkey = dir.path().join("public.pem");
    let signature = dir.path().join("signature");
    let message = dir.path().join("message");
    fs::write(&pubkey, PUBLIC).unwrap();
    fs::write(&signature, URL_SAFE_NO_PAD.decode(pieces[2]).unwrap()).unwrap();
    fs::write(&message, format!("{}.{}", pieces[0], pieces[1])).unwrap();
    let output = Command::new("openssl")
        .args(["dgst", "-sha256", "-verify"])
        .arg(pubkey)
        .arg("-signature")
        .arg(signature)
        .arg(message)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!format!("{credentials:?}").contains("BEGIN"));
    assert!(credentials.jwt(0).is_err());
    assert!(credentials.jwt(u64::MAX).is_err());
    assert!(
        AppCredentials::from_pem(7, "client".into(), b"invalid-private-material".to_vec()).is_err()
    );
}

#[test]
fn binding_rejects_url_injection_and_unpinned_ids() {
    for invalid in [
        "..", ".", "a/b", "a%2fb", "a?b", "a#b", "a\\b", "a\nb", "", "é",
    ] {
        let mut b = binding();
        b.name = invalid.into();
        assert!(b.validate().is_err());
    }
    let mut b = binding();
    b.repository_id = 0;
    assert!(b.validate().is_err());
}

#[tokio::test]
async fn production_transport_requires_https() {
    let client = client(true).unwrap();
    assert!(
        client
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .unwrap_err()
            .is_builder()
    );
}

#[tokio::test]
async fn slow_response_has_a_deadline_and_releases_its_probe_slot() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 4096];
        assert!(socket.read(&mut bytes).await.unwrap() > 0);
        // Keep an accepted connection alive without producing headers.
        tokio::time::sleep(Duration::from_secs(20)).await;
        drop(socket);
    });
    let connection = Connection {
        client: client(false).unwrap(),
        base,
        credentials: credentials(),
        slots: Arc::new(Semaphore::new(1)),
    };
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(18), connection.probe(&binding()))
        .await
        .unwrap();
    assert_eq!(
        result.unwrap_err(),
        Error::Transport {
            stage: Stage::Installation
        }
    );
    assert!(started.elapsed() < Duration::from_secs(18));
    assert_eq!(connection.slots.available_permits(), 1);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

/// Optional real TLS/API compatibility check, not the authenticated App gate.
#[tokio::test]
#[ignore = "requires public GitHub network access; uses no credentials"]
async fn live_public_github_https_api() {
    let mut response = client(true)
        .unwrap()
        .get("https://api.github.com/repos/ArtAndrew/TreeHarbor")
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", API_VERSION)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-github-api-version-selected")
            .unwrap(),
        API_VERSION
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.unwrap() {
        assert!(bytes.len() + chunk.len() <= MAX_RESPONSE);
        bytes.extend_from_slice(&chunk);
    }
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["id"], 1_428_127_654_u64);
    assert_eq!(body["full_name"], "ArtAndrew/TreeHarbor");
}

#[path = "tests/queue.rs"]
mod queue_tests;

#[path = "tests/deliveries.rs"]
mod delivery_tests;
