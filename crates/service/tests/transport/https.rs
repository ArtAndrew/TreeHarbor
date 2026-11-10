use super::*;
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, ServerName, pem::PemObject},
};
use std::{net::SocketAddr, path::Path, sync::Arc};
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};

fn openssl(dir: &Path, args: &[&str]) {
    let output = Command::new("openssl")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "openssl {} failed: {}",
        args[0],
        String::from_utf8_lossy(&output.stderr)
    );
}

fn ca(dir: &Path, prefix: &str) -> PathBuf {
    let key = format!("{prefix}.key");
    let certificate = format!("{prefix}.pem");
    openssl(
        dir,
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            &format!("/CN={prefix}"),
            "-keyout",
            &key,
            "-out",
            &certificate,
            "-config",
            "ca.cnf",
        ],
    );
    for name in [&key, &certificate] {
        fs::set_permissions(dir.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    dir.join(certificate)
}

fn settings(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    fs::write(dir.join("ca.cnf"), "[req]\ndistinguished_name=dn\nx509_extensions=ca\n[dn]\n[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign\n").unwrap();
    let trust = ca(dir, "treeharbor-test-ca");
    let other = ca(dir, "unrelated-test-ca");
    openssl(
        dir,
        &[
            "req",
            "-new",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            "/CN=treeharbor-test",
            "-keyout",
            "tls.key",
            "-out",
            "tls.csr",
        ],
    );
    fs::write(dir.join("leaf.cnf"), "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n").unwrap();
    openssl(
        dir,
        &[
            "x509",
            "-req",
            "-in",
            "tls.csr",
            "-CA",
            "treeharbor-test-ca.pem",
            "-CAkey",
            "treeharbor-test-ca.key",
            "-CAcreateserial",
            "-days",
            "1",
            "-extfile",
            "leaf.cnf",
            "-out",
            "tls.pem",
        ],
    );
    for file in ["tls.key", "tls.pem"] {
        fs::set_permissions(dir.join(file), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let path = dir.join("https.json");
    files::write_private(&path, &serde_json::to_vec(&json!({"version":1,"listen":"127.0.0.1:0",
        "server_name":"127.0.0.1","certificate":dir.join("tls.pem"),"private_key":dir.join("tls.key")})).unwrap()).unwrap();
    (path, trust, other)
}

fn address(f: &Fixture) -> SocketAddr {
    let log = fs::read_to_string(f.dir.path().join("server.log")).unwrap();
    let ready: Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    ready["https"].as_str().unwrap().parse().unwrap()
}

fn remote(f: &Fixture, url: &str, trust: &Path, args: &[&str]) -> Output {
    Command::new(&f.binary)
        .env("PATH", "/treeharbor-test-no-executables")
        .args(["client", "--https", url, "--ca-cert"])
        .arg(trust)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(args)
        .output()
        .unwrap()
}

fn ok(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.windows(5).any(|bytes| bytes == b"thv1_"));
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["result"].clone()
}

async fn tls(addr: SocketAddr, ca: &Path) -> TlsStream<TcpStream> {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_file_iter(ca).unwrap() {
        roots.add(cert.unwrap()).unwrap();
    }
    let config = ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
        .connect(
            ServerName::try_from("127.0.0.1").unwrap(),
            TcpStream::connect(addr).await.unwrap(),
        )
        .await
        .unwrap()
}

fn request(addr: SocketAddr, credential: &Credential, path: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!("POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", credential.token.expose(), body.len()).into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

async fn response_status(addr: SocketAddr, ca: &Path, request: &[u8]) -> u16 {
    let mut stream = tls(addr, ca).await;
    stream.write_all(request).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(!response.windows(5).any(|bytes| bytes == b"thv1_"));
    let first = response.split(|&byte| byte == b'\n').next().unwrap();
    std::str::from_utf8(first)
        .unwrap()
        .split_ascii_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL and OpenSSL; uses actual CLI, TCP/TLS and object storage"]
async fn https_agent_reads_and_replays_durable_changes_with_scoped_authority() {
    let authority = Authority::Connected.as_str();
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    f.importer();
    let (settings, trust, other_ca) = settings(f.dir.path());
    let mut server = f.start_with_https(Some(&settings)).await;
    let addr = address(&f);
    let url = format!("https://{addr}");
    let importer = f.credential();
    let dir = f.dir.path().join("input");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("main.rs"), b"original\n").unwrap();
    let pack = f.dir.path().join("pack");
    let output = Command::new(&f.binary)
        .args(["source-pack", "--directory"])
        .arg(&dir)
        .arg("--output")
        .arg(&pack)
        .args([
            "--path",
            "main.rs",
            "--author-name",
            "Test",
            "--author-email",
            "test@example.invalid",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let imported = ok(f.cli(&[
        "source-import",
        "--pack",
        pack.to_str().unwrap(),
        "--state",
        f.dir.path().join("import.json").to_str().unwrap(),
        "--authority",
        authority,
        "--epoch",
        "1",
    ]));
    let profile = ok(f.cli(&[
        "profile-put",
        "--key",
        "full",
        "--authority",
        authority,
        "--epoch",
        "1",
        "--full",
    ]));

    assert!(
        !remote(&f, &url, &other_ca, &["repository-info"])
            .status
            .success()
    );
    assert!(
        !remote(
            &f,
            &format!("https://localhost:{}", addr.port()),
            &trust,
            &["repository-info"]
        )
        .status
        .success()
    );
    let forbidden = importer.request(json!({"method":"source_begin","key":"https-import","authority":authority,"epoch":1,"upload":Uuid::new_v4(),"max_objects":10,"max_bytes":1024,"ttl_seconds":60}));
    assert_eq!(
        response_status(
            addr,
            &trust,
            &request(
                addr,
                &importer,
                "/v1/commands",
                &serde_json::to_vec(&forbidden).unwrap()
            )
        )
        .await,
        403
    );
    assert_eq!(
        response_status(
            addr,
            &trust,
            &request(addr, &importer, "/v1/native/prepare", b"{}")
        )
        .await,
        404
    );
    assert_eq!(
        response_status(
            addr,
            &trust,
            &request(addr, &importer, "/v1/commands", &vec![b' '; 65537])
        )
        .await,
        413
    );
    for (field, value) in [
        ("tenant", json!("other-tenant")),
        ("repository", json!(Uuid::new_v4())),
    ] {
        let mut cross = importer.request(json!({"method":"repository_info"}));
        cross[field] = value;
        let denial = response_status(
            addr,
            &trust,
            &request(
                addr,
                &importer,
                "/v1/commands",
                &serde_json::to_vec(&cross).unwrap(),
            ),
        )
        .await;
        assert!(
            matches!(denial, 401 | 403),
            "unexpected response for wrong {field}: {denial}"
        );
    }
    let mut plaintext = TcpStream::connect(addr).await.unwrap();
    plaintext
        .write_all(format!("GET /healthz HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut prefix = [0; 5];
    let result = tokio::time::timeout(Duration::from_secs(2), plaintext.read(&mut prefix))
        .await
        .unwrap();
    assert!(result.is_err() || prefix != *b"HTTP/");
    let mut idle = TcpStream::connect(addr).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(8), idle.read(&mut prefix))
        .await
        .expect("idle TLS handshake was not closed");
    assert!(matches!(result, Ok(0) | Err(_)));

    let scope = f.scope.repository().to_string();
    let permissions = [
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &scope,
        "--principal",
        "agent-a",
        "--permission",
        "read",
        "--permission",
        "workspace.write",
        "--permission",
        "change.write",
    ];
    f.admin_ok(&[&["access-set"][..], &permissions].concat());
    let agent = f.dir.path().join("agent-https.json");
    f.admin_ok(
        &[
            &["token-issue"][..],
            &permissions,
            &["--output", agent.to_str().unwrap()],
        ]
        .concat(),
    );
    f.credential_path = agent;
    let intent = f.dir.path().join("agent-intent.json");
    let create = [
        "api-run-new",
        "--state",
        intent.to_str().unwrap(),
        "--authority",
        authority,
        "--epoch",
        "1",
        "--base",
        imported["revision"].as_str().unwrap(),
        "--profile-digest",
        profile["profile_digest"].as_str().unwrap(),
        "--agent",
        "remote-controller",
        "--task",
        "replace-file",
    ];
    let run = ok(remote(&f, &url, &trust, &create));
    assert_eq!(run, ok(remote(&f, &url, &trust, &create)));
    assert_eq!(run["execution_observed"], false);
    let workspace = run["workspace"].as_str().unwrap();
    let run_id = run["run"].as_str().unwrap();
    let download = f.dir.path().join("download.rs");
    let read = ok(remote(
        &f,
        &url,
        &trust,
        &[
            "source-file",
            "--authority",
            authority,
            "--epoch",
            "1",
            "--workspace",
            workspace,
            "--generation",
            "1",
            "--path",
            "main.rs",
            "--output",
            download.to_str().unwrap(),
        ],
    ));
    assert_eq!(fs::read(&download).unwrap(), b"original\n");
    let upload = Uuid::new_v4().to_string();
    ok(remote(
        &f,
        &url,
        &trust,
        &[
            "change-begin",
            "--key",
            "begin",
            "--authority",
            authority,
            "--epoch",
            "1",
            "--upload",
            &upload,
            "--run",
            run_id,
            "--generation",
            "1",
        ],
    ));
    let replacement = f.dir.path().join("replacement");
    files::write_private(&replacement, b"new contents\0\xff").unwrap();
    let put = ok(remote(
        &f,
        &url,
        &trust,
        &[
            "change-put",
            "--upload",
            &upload,
            "--file",
            replacement.to_str().unwrap(),
        ],
    ));
    let operations = json!([{"path_hex":"6d61696e2e7273","expected":{"mode":33188,"object":read["object"]},
        "replacement":{"mode":33188,"object":put["object"]}}]);
    let manifest = f.dir.path().join("changes.json");
    files::write_private(&manifest, &serde_json::to_vec(&operations).unwrap()).unwrap();
    let change = Uuid::new_v4().to_string();
    let body = json!({"version":1,"tenant":f.scope.tenant(),"repository":scope,"key":"submit","authority":authority,"epoch":1,
        "upload":upload,"changeset":change,"previous_version":0,"operations":operations});
    let mut lost = tls(addr, &trust).await;
    lost.write_all(&request(
        addr,
        &f.credential(),
        "/v1/changes/submit",
        &serde_json::to_vec(&body).unwrap(),
    ))
    .await
    .unwrap();
    // Leave the TLS response unread and wait for the independent database proof.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let count: i64 = f
            .sql
            .query_one("SELECT count(*) FROM th_operations WHERE key='submit'", &[])
            .await
            .unwrap()
            .get(0);
        if count == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "remote submission did not commit"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(lost);
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    server = f.start_with_https(Some(&settings)).await;
    let url = format!("https://{}", address(&f));
    let submit = [
        "change-submit",
        "--key",
        "submit",
        "--authority",
        authority,
        "--epoch",
        "1",
        "--upload",
        &upload,
        "--changeset",
        &change,
        "--operations",
        manifest.to_str().unwrap(),
    ];
    let accepted = ok(remote(&f, &url, &trust, &submit));
    assert_eq!(accepted["run"], run_id);
    assert_eq!(accepted["run_origin"], "external_api");
    assert_eq!(accepted["state"], "accepted");
    ok(remote(
        &f,
        &url,
        &trust,
        &[
            "api-run-close",
            "--key",
            "close",
            "--authority",
            authority,
            "--epoch",
            "1",
            "--run",
            run_id,
            "--generation",
            "1",
        ],
    ));
    assert_eq!(ok(remote(&f, &url, &trust, &submit)), accepted);
    let retained = ok(remote(
        &f,
        &url,
        &trust,
        &[
            "diff",
            "--authority",
            authority,
            "--epoch",
            "1",
            "change",
            "--changeset",
            &change,
            "--version",
            "1",
        ],
    ));
    assert_eq!(retained["changes"][0]["after"]["object"], put["object"]);
    assert_eq!(
        f.sql
            .query_one("SELECT count(*) FROM th_change_versions", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    let token: Value = serde_json::from_slice(&fs::read(&f.credential_path).unwrap()).unwrap();
    f.admin_ok(&[
        "token-revoke",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &scope,
        "--token-id",
        token["token_id"].as_str().unwrap(),
    ]);
    let revoked = remote(&f, &url, &trust, &["repository-info"]);
    assert!(!revoked.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&revoked.stdout).unwrap()["error"]["code"],
        "unauthenticated"
    );
    assert!(
        !fs::read_to_string(f.dir.path().join("server.log"))
            .unwrap()
            .contains("thv1_")
    );
    drop(server);
    f.cleanup().await;
}
