use super::*;

pub(super) async fn exercise(f: &mut Fixture, server: &mut Process, binding: Uuid, manifest: &str) {
    let row=f.sql.query_one("SELECT d.dequeue,q.queue FROM th_github_dequeues d JOIN th_github_queue q USING(tenant,repository,queue) WHERE q.binding=$1 ORDER BY d.dequeue LIMIT 1",&[&binding]).await.unwrap();
    let dequeue: Uuid = row.get(0);
    let queue: Uuid = row.get(1);
    for (kind, operation) in [("queue", queue), ("dequeue", dequeue)] {
        let subject = json!({"kind":kind,"operation":operation});
        let bundle = result(request(
            f,
            &json!({"action":"restore_queue_context","epoch":1,"binding":binding,"manifest_sha256":manifest,"subject":subject}),
        ));
        assert_eq!(bundle["queue"], queue.to_string());
        let t = &bundle["target"]["pull"];
        // Synthetic host snapshot; the adapter's separate HTTP tests cover reads.
        let snapshot = json!({"pull":{"repository_id":t["repository_id"],"number":t["pull_number"],"node_id":t["pull_node"],"head_ref":t["head_ref"],"base_ref":t["base_ref"],"head_revision":t["head_revision"],"base_revision":bundle["target"]["expected_base_revision"],"state":"open","draft":false,"merged":false},"queue_enabled":true,"queue_id":"MQ_recovery","configuration":null,"entry":null,"final_commit":null});
        let now: i64 = f
            .sql
            .query_one(
                "SELECT (extract(epoch FROM clock_timestamp())*1000)::bigint",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        let spec = json!({"version":1,"binding":binding,"manifest_sha256":manifest,"context_sha256":bundle["context_sha256"],"subject":subject,
            "report":{"version":1,"app_id":bundle["context"]["specification"]["app_id"],"installation_id":bundle["context"]["specification"]["upstream"]["installation_id"],"started_at_unix_ms":now,"finished_at_unix_ms":now,"before":snapshot,"after":snapshot,"token_revoked":true}});
        let state = f.dir.path().join(format!("restore-{kind}-capture"));
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let intent = json!({"version":1,"kind":"restore_queue","tenant":f.scope.tenant(),"repository":f.scope.repository(),"epoch":1,"binding":binding,"manifest_sha256":manifest,"subject":subject});
        files::write_private(&state.join("intent.json"), intent.to_string().as_bytes()).unwrap();
        let key = format!("github-publish-report-restore-{kind}-fixture");
        files::write_private(
            &state.join("report-0000.json"),
            json!({"version":1,"key":key,"report":spec,"token_expires_at":null})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        let saved = result(request(
            f,
            &json!({"action":"restore_queue_record","epoch":1,"key":key,"specification":spec}),
        ));
        assert_eq!(saved["prior_mutation_settled"], false);
        assert_eq!(saved["classification"], "not_in_queue");
        assert!(!state.join("ack-0000.json").exists());
        server.0.kill().unwrap();
        server.0.wait().unwrap();
        *server = f.start().await;
        for _ in 0..2 {
            let output = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
                .env("PATH", "/no-executables")
                .args(["github", "restore-inspect-queue", "--socket"])
                .arg(&f.config.socket)
                .arg("--credential-file")
                .arg(&f.credential_path)
                .args([
                    "--epoch",
                    "1",
                    "--binding",
                    &binding.to_string(),
                    "--manifest-sha256",
                    manifest,
                    "--kind",
                    kind,
                    "--operation",
                    &operation.to_string(),
                ])
                .arg("--state")
                .arg(&state)
                .arg("--config")
                .arg(f.dir.path().join("absent-app.json"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                saved
            );
        }
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(state.join("ack-0000.json")).unwrap())
                .unwrap(),
            saved
        );
        assert_eq!(f.sql.query_one("SELECT count(*) FROM th_operations WHERE key=$1 AND kind='github.restore.queue.v1'",&[&key]).await.unwrap().get::<_,i64>(0),1);
    }
}
