use super::*;
use std::{path::Path, process::Stdio};

fn new_command(f: &Fixture, socket: &Path, state: &Path, base: &str, mode: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    command
        .env("PATH", "/no-executables")
        .args(["client", "--socket"])
        .arg(socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(["api-run-new", "--state"])
        .arg(state)
        .args([
            "--agent",
            "controller/1",
            "--task",
            "fix-parser",
            "--authority",
            mode,
            "--epoch",
            "1",
            "--base",
            base,
            "--profile-digest",
            &treeharbor_core::SourceProfile::full().id().to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

use super::workspace_new::message;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; actual CLI loss/restart/concurrent retry"]
async fn api_run_new_keeps_one_task_after_lost_reply_restart_and_close() {
    for authority in [Authority::Connected, Authority::Standalone] {
        let f = Fixture::new_with_authority(authority).await;
        let base = f.seed_with_authority(authority).await.to_string();
        let mut server = f.start().await;
        let profile = f.cli(&[
            "profile-put",
            "--key",
            "new-profile",
            "--authority",
            authority.as_str(),
            "--epoch",
            "1",
            "--full",
        ]);
        assert!(
            profile.status.success(),
            "{}",
            String::from_utf8_lossy(&profile.stderr)
        );
        let socket = f.dir.path().join("drop.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let upstream = f.config.socket.clone();
        let (signal, received) = tokio::sync::oneshot::channel();
        let proxy = tokio::spawn(async move {
            let (mut incoming, _) = listener.accept().await.unwrap();
            let request = message(&mut incoming).await;
            assert!(String::from_utf8_lossy(&request).contains("\"method\":\"api_run_create\""));
            let mut outgoing = UnixStream::connect(upstream).await.unwrap();
            outgoing.write_all(&request).await.unwrap();
            let response = message(&mut outgoing).await;
            assert!(String::from_utf8_lossy(&response).contains("\"state\":\"recorded\""));
            signal.send(()).unwrap();
            // A real DB commit has happened; the CLI never receives its reply.
            std::future::pending::<()>().await;
        });
        let state = f.dir.path().join("first-task.json");
        let mut child = new_command(&f, &socket, &state, &base, authority.as_str())
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), received)
            .await
            .unwrap()
            .unwrap();
        assert!(child.try_wait().unwrap().is_none());
        let intent_bytes = fs::read(&state).unwrap();
        let intent: Value = serde_json::from_slice(&intent_bytes).unwrap();
        assert!(!String::from_utf8_lossy(&intent_bytes).contains("thv1_"));
        child.kill().unwrap();
        child.wait().unwrap();
        proxy.abort();
        let _ = proxy.await;
        server.0.kill().unwrap();
        server.0.wait().unwrap();
        server = f.start().await;

        let replay = new_command(&f, &f.config.socket, &state, &base, authority.as_str())
            .output()
            .unwrap();
        assert!(
            replay.status.success(),
            "{}",
            String::from_utf8_lossy(&replay.stderr)
        );
        let replay: Value = serde_json::from_slice(&replay.stdout).unwrap();
        assert_eq!(replay["result"]["workspace"], intent["workspace"]);
        assert_eq!(replay["result"]["run"], intent["run"]);
        assert_eq!(replay["result"]["origin"], "external_api");
        assert_eq!(replay["result"]["execution_observed"], false);
        assert_eq!(replay["result"]["state"], "recorded");
        assert_eq!(replay["result"]["base"], base);
        assert_eq!(fs::read(&state).unwrap(), intent_bytes);

        // Changed task inputs fail against an absent socket, before network IO.
        let changed_base = format!("native:sha256:{}", "02".repeat(32));
        let bad = new_command(
            &f,
            &f.dir.path().join("absent.sock"),
            &state,
            &changed_base,
            authority.as_str(),
        )
        .output()
        .unwrap();
        assert!(!bad.status.success());
        assert!(String::from_utf8_lossy(&bad.stderr).contains("matching private state"));
        assert_eq!(fs::read(&state).unwrap(), intent_bytes);

        let next_state = f.dir.path().join("second-task.json");
        let a = new_command(&f, &f.config.socket, &next_state, &base, authority.as_str())
            .spawn()
            .unwrap();
        let b = new_command(&f, &f.config.socket, &next_state, &base, authority.as_str())
            .spawn()
            .unwrap();
        let (a, b) = (a.wait_with_output().unwrap(), b.wait_with_output().unwrap());
        assert!(a.status.success(), "{}", String::from_utf8_lossy(&a.stderr));
        assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
        let a: Value = serde_json::from_slice(&a.stdout).unwrap();
        let b: Value = serde_json::from_slice(&b.stdout).unwrap();
        assert_eq!(a, b);
        assert_ne!(a["result"]["workspace"], replay["result"]["workspace"]);
        let count: i64 = f
            .sql
            .query_one("SELECT count(*) FROM th_workspaces", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 2);
        let count: i64 = f
            .sql
            .query_one(
                "SELECT count(*) FROM th_outbox WHERE kind IN ('workspace.prepare','run.execute','workspace.drain')",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 0);
        let count: i64 = f
            .sql
            .query_one("SELECT count(*) FROM th_agent_runs", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 2);

        let run = replay["result"]["run"].as_str().unwrap();
        let closed = f.cli(&[
            "api-run-close",
            "--key",
            "close-first",
            "--authority",
            authority.as_str(),
            "--epoch",
            "1",
            "--run",
            run,
            "--generation",
            "1",
        ]);
        assert!(
            closed.status.success(),
            "{}",
            String::from_utf8_lossy(&closed.stderr)
        );
        let retry = new_command(&f, &f.config.socket, &state, &base, authority.as_str())
            .output()
            .unwrap();
        assert!(
            retry.status.success(),
            "{}",
            String::from_utf8_lossy(&retry.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&retry.stdout).unwrap(),
            replay
        );
        assert_eq!(fs::read(&state).unwrap(), intent_bytes);
        let status = f.cli(&["api-run-status", "--run", run]);
        assert!(status.status.success());
        let status: Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(status["result"]["state"], "closed");
        assert_eq!(status["result"]["workspace_state"], "deleted");
        assert_eq!(status["result"]["execution_observed"], false);
        drop(server);
        f.cleanup().await;
    }
}
