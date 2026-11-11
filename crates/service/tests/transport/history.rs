use super::*;
use std::collections::{BTreeMap, BTreeSet};
use treeharbor_core::ObjectId;
fn ok(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["result"]["history"].clone()
}
fn history(f: &Fixture, args: &[&str]) -> Output {
    let mut all = vec!["history", "--authority", "standalone", "--epoch", "1"];
    all.extend_from_slice(args);
    f.cli(&all)
}
fn git_commit(repo: &std::path::Path, tree: &str, parents: &[&str], name: &str) -> String {
    let ancestry: String = parents.iter().map(|p| format!("parent {p}\n")).collect();
    git_reference::hash(repo,"commit",format!("tree {tree}\n{ancestry}author Test <test@example.invalid> 1234567890 +0000\ncommitter Other <other@example.invalid> 1234567890 +0000\n\n{name}\n").as_bytes())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual history CLI, Git export and credential boundaries"]
async fn history_cli_preserves_criss_cross_pagination_and_scoped_reads_after_restart() {
    let mut f = Fixture::new().await;
    f.importer();
    let repo = f.dir.path().join("oracle.git");
    git_reference::git(
        f.dir.path(),
        &["init", "--bare", "--template=", repo.to_str().unwrap()],
        b"",
    );
    let contents = b"retained\0bytes\r\n";
    let blob = git_reference::hash(&repo, "blob", contents);
    let tree = String::from_utf8(git_reference::git(
        &repo,
        &["mktree"],
        format!("100755 blob {blob}\tfile.bin\n").as_bytes(),
    ))
    .unwrap();
    let tree = tree.trim();
    let root = git_commit(&repo, tree, &[], "root");
    let a = git_commit(&repo, tree, &[&root], "a");
    let b = git_commit(&repo, tree, &[&root], "b");
    let left = git_commit(&repo, tree, &[&a, &b], "left");
    let right = git_commit(&repo, tree, &[&b, &a], "right");
    let disconnected = git_commit(&repo, tree, &[], "independent");
    let oracle_bases: BTreeSet<_> = String::from_utf8(git_reference::git(
        &repo,
        &["merge-base", "--all", &left, &right],
        b"",
    ))
    .unwrap()
    .lines()
    .map(str::to_owned)
    .collect();
    assert_eq!(oracle_bases, BTreeSet::from([a.clone(), b.clone()]));
    let oracle_first: Vec<_> = String::from_utf8(git_reference::git(
        &repo,
        &["rev-list", "--first-parent", &left],
        b"",
    ))
    .unwrap()
    .lines()
    .map(str::to_owned)
    .collect();
    let mut server = f.start().await;
    let mut roots = BTreeMap::new();
    let mut origins = BTreeMap::new();
    let mut commits = BTreeMap::new();
    for (name, oid) in [
        ("left", &left),
        ("right", &right),
        ("disconnected", &disconnected),
    ] {
        let bundle = f.dir.path().join(format!("{name}.bundle"));
        git_reference::bundle(&repo, &bundle, oid);
        let pack_path = f.dir.path().join(format!("{name}-pack"));
        let captured = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
            .args(["git-import", "--bundle"])
            .arg(&bundle)
            .arg("--commit")
            .arg(format!("git:sha1:{oid}"))
            .arg("--output")
            .arg(&pack_path)
            .output()
            .unwrap();
        assert!(
            captured.status.success(),
            "{}",
            String::from_utf8_lossy(&captured.stderr)
        );
        let pack = treeharbor_service::pack::Pack::open(&pack_path).unwrap();
        let manifest: Value =
            serde_json::from_slice(&fs::read(pack_path.join("manifest.json")).unwrap()).unwrap();
        for value in manifest["objects"].as_array().unwrap() {
            let id: ObjectId = value.as_str().unwrap().parse().unwrap();
            if let Object::NativeCommit(c) =
                Object::decode_verified(id, &pack.frame(id).unwrap()).unwrap()
            {
                let git = RevisionRef::Git(c.git_origin.as_ref().unwrap().oid).to_string();
                let rev = RevisionRef::Native(id).to_string();
                origins.insert(git, rev.clone());
                commits.insert(rev, c);
            }
        }
        let state = f.dir.path().join(format!("{name}-upload.json"));
        let imported = f.cli(&[
            "source-import",
            "--pack",
            pack_path.to_str().unwrap(),
            "--state",
            state.to_str().unwrap(),
            "--authority",
            "standalone",
            "--epoch",
            "1",
        ]);
        assert!(
            imported.status.success(),
            "{}",
            String::from_utf8_lossy(&imported.stderr)
        );
        roots.insert(name, RevisionRef::Native(pack.commit()).to_string());
        fs::remove_dir_all(pack_path).unwrap();
        fs::remove_file(bundle).unwrap();
    }
    let branches = BTreeMap::from([
        ("left", "left"),
        ("right", "right"),
        ("independent", "disconnected"),
        ("archive/left", "left"),
    ]);
    f.credential_path = native::role(
        &mut f,
        "history-owner",
        Some("human"),
        &["repository.write", "policy.write", "read"],
    );
    let vector: Value =
        serde_json::from_str(include_str!("../fixtures/native-ref-snapshot-v1.json")).unwrap();
    let capture = |fixture: &Fixture| native::native(fixture, vector["request"].clone());
    let mut empty = vector["empty"].clone();
    empty["tenant"] = json!(f.scope.tenant());
    empty["repository"] = json!(f.scope.repository());
    assert_eq!(native::ok(capture(&f)), empty);
    let digest = ObjectId::from_bytes([7; 32]).to_string();
    let policy = native::ok(native::native(
        &f,
        json!({
            "action":"policy_put","key":"history-policy","epoch":1,
            "policy":{"version":1,"checks":[{"name":"tests","definition":digest,"environment":digest,"toolchain":digest,"dependencies":digest,"validators":["ci"]}],"reviewers":["reviewer"],"approvals":1}
        }),
    ));
    for (branch, root) in &branches {
        let ref_state = native::ok(native::native(
            &f,
            json!({
                "action":"ref_create","key":Uuid::new_v4(),"epoch":1,
                "name":branch,"head":roots[root],"policy":policy["policy"]
            }),
        ));
        assert_eq!(ref_state["head"], roots[root]);
    }
    fs::remove_dir_all(repo).unwrap();
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    server = f.start().await;
    // Actual read-only credential, not an admin/writer token.
    f.admin_ok(&[
        "principal-create",
        "--tenant",
        f.scope.tenant(),
        "--principal",
        "history-reader",
        "--kind",
        "human",
    ]);
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "history-reader",
        "--permission",
        "read",
    ]);
    f.credential_path = f.dir.path().join("history-reader.json");
    let issued = f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "history-reader",
        "--permission",
        "read",
        "--output",
        f.credential_path.to_str().unwrap(),
    ]);
    let left = &roots["left"];
    let right = &roots["right"];
    let disconnected = &roots["disconnected"];
    let held_before: i64 = f
        .sql
        .query_one("SELECT count(*) FROM th_object_holds", &[])
        .await
        .unwrap()
        .get(0);
    let mut export = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    export.arg("git-export");
    let captured = native::ok(capture(&f));
    let captured_refs = captured["refs"].as_array().unwrap();
    assert_eq!(captured_refs.len(), branches.len());
    assert_eq!(captured["epoch"], 1);
    for name in ["left", "right", "disconnected"] {
        let pack = f.dir.path().join(format!("export={name}"));
        let downloaded = f.cli(&[
            "source-export",
            "--authority",
            "standalone",
            "--epoch",
            "1",
            "--revision",
            &roots[name],
            "--output",
            pack.to_str().unwrap(),
        ]);
        assert!(
            downloaded.status.success(),
            "{}",
            String::from_utf8_lossy(&downloaded.stderr)
        );
    }
    for (state, (branch, root)) in captured_refs.iter().zip(&branches) {
        assert_eq!(state["name"], *branch);
        assert_eq!(state["head"], roots[root]);
        assert_eq!(
            state["snapshot"],
            commits[&roots[root]].snapshot.to_string()
        );
        assert_eq!(state["generation"], 1);
        assert_eq!(state["policy"], policy["policy"]);
        assert_eq!(state["epoch"], captured["epoch"]);
        let pack = f.dir.path().join(format!("export={root}"));
        export
            .arg("--ref")
            .arg(format!("{branch}={}", pack.display()));
    }
    let projected = f.dir.path().join("multi-branch-git");
    let result = export.arg("--output").arg(&projected).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let manifest: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        manifest["commits"].as_object().unwrap().len(),
        commits.len()
    );
    assert_eq!(manifest["refs"].as_object().unwrap().len(), branches.len());
    let live_bundle = f.dir.path().join("live-branch-git");
    let live = f.cli(&[
        "git-export",
        "--epoch",
        "1",
        "--output",
        live_bundle.to_str().unwrap(),
    ]);
    assert!(
        live.status.success(),
        "{} {}",
        String::from_utf8_lossy(&live.stdout),
        String::from_utf8_lossy(&live.stderr)
    );
    let live: Value = serde_json::from_slice(&live.stdout).unwrap();
    assert_eq!(live["snapshot"], captured);
    assert_eq!(live["export"]["refs"], manifest["refs"]);
    assert_eq!(live["export"]["commits"], manifest["commits"]);
    let stale_bundle = f.dir.path().join("stale-branch-git");
    let stale = f.cli(&[
        "git-export",
        "--epoch",
        "2",
        "--output",
        stale_bundle.to_str().unwrap(),
    ]);
    assert!(!stale.status.success());
    assert!(!stale_bundle.exists());
    assert!(fs::read_dir(f.dir.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".treeharbor-branch-packs-")
    }));
    let restored = f.dir.path().join("restored.git");
    git_reference::git(
        f.dir.path(),
        &[
            "clone",
            "--bare",
            live_bundle.join("history.bundle").to_str().unwrap(),
            restored.to_str().unwrap(),
        ],
        b"",
    );
    git_reference::git(&restored, &["fsck", "--strict"], b"");
    for (branch, root) in &branches {
        let name = format!("refs/heads/{branch}");
        let revision = &roots[root];
        let expected =
            RevisionRef::Git(commits[revision].git_origin.as_ref().unwrap().oid).to_string();
        let expected = expected.strip_prefix("git:sha1:").unwrap();
        assert_eq!(manifest["refs"][&name]["git"], expected);
        assert_eq!(
            String::from_utf8(git_reference::git(&restored, &["rev-parse", &name], b""))
                .unwrap()
                .trim(),
            expected
        );
        assert_eq!(
            git_reference::git(&restored, &["show", &format!("{name}:file.bin")], b""),
            contents
        );
    }
    let shared = &origins[&format!("git:sha1:{a}")];
    let RevisionRef::Native(shared) = shared.parse().unwrap() else {
        panic!()
    };
    let hex = shared.hex();
    let right_pack = f.dir.path().join("export=right");
    fs::remove_file(
        right_pack
            .join("objects/native-pack")
            .join(&hex[..2])
            .join(&hex[2..]),
    )
    .unwrap();
    let incomplete = f.dir.path().join("incomplete-export");
    let failed = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .args(["git-export", "--ref"])
        .arg(format!(
            "left={}",
            f.dir.path().join("export=left").display()
        ))
        .arg("--ref")
        .arg(format!("right={}", right_pack.display()))
        .arg("--output")
        .arg(&incomplete)
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(!incomplete.exists());
    f.sql.execute(r#"INSERT INTO th_native_refs(tenant,repository,name,head,generation,epoch,policy,pin_holder)
        SELECT r.tenant,r.repository,'fixture-ref-' || i,r.head,r.generation,r.epoch,r.policy,r.pin_holder
        FROM th_native_refs r CROSS JOIN generate_series(1,125) AS i
        WHERE r.tenant=$1 AND r.repository=$2 AND r.name='left'"#,
        &[&f.scope.tenant(), &f.scope.repository()],
    ).await.unwrap();
    let overflow = capture(&f);
    assert!(!overflow.status.success());
    let overflow: Value = serde_json::from_slice(&overflow.stdout).unwrap();
    assert_eq!(overflow["error"]["code"], "native_ref_snapshot_limit");
    assert!(overflow.get("result").is_none());
    f.sql.execute("DELETE FROM th_native_refs WHERE tenant=$1 AND repository=$2 AND name LIKE 'fixture-ref-%'",
        &[&f.scope.tenant(), &f.scope.repository()]).await.unwrap();
    let bases = ok(history(
        &f,
        &["merge-bases", "--left", left, "--right", right],
    ));
    let expected: BTreeSet<_> = oracle_bases
        .iter()
        .map(|oid| origins[&format!("git:sha1:{oid}")].clone())
        .collect();
    assert_eq!(
        bases["bases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>(),
        expected
    );
    let mut after: Option<String> = None;
    let mut rows = Vec::new();
    loop {
        let mut args = vec!["log", "--revision", left, "--limit", "1"];
        if let Some(cursor) = &after {
            args.extend(["--after", cursor]);
        }
        let page = ok(history(&f, &args));
        assert_eq!(page["total_commits"], 4);
        assert_eq!(page["order"], "children_before_parents_id_v1");
        let page_rows = page["commits"].as_array().unwrap();
        assert_eq!(page_rows.len(), 1);
        rows.extend(page_rows.clone());
        after = page["next_after"].as_str().map(str::to_owned);
        if after.is_none() {
            break;
        }
        assert!(rows.len() < 4);
    }
    let positions: BTreeMap<_, _> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r["revision"].as_str().unwrap().to_owned(), i))
        .collect();
    assert_eq!(positions.len(), 4);
    for row in &rows {
        let rev = row["revision"].as_str().unwrap();
        let c = &commits[rev];
        assert_eq!(row["message"], c.message);
        assert_eq!(
            row["parents"],
            json!(
                c.parents
                    .iter()
                    .map(|id| RevisionRef::Native(*id).to_string())
                    .collect::<Vec<_>>()
            )
        );
        for parent in &c.parents {
            assert!(positions[rev] < positions[&RevisionRef::Native(*parent).to_string()]);
        }
        assert_eq!(
            ok(history(&f, &["show", "--revision", left, "--commit", rev]))["commit"],
            *row
        );
    }
    let first = ok(history(&f, &["log", "--revision", left, "--first-parent"]));
    let expected_first: Vec<_> = oracle_first
        .iter()
        .map(|oid| origins[&format!("git:sha1:{oid}")].clone())
        .collect();
    assert_eq!(
        first["commits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["revision"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        expected_first
    );
    assert_eq!(
        ok(history(
            &f,
            &["is-ancestor", "--ancestor", left, "--head", left]
        ))["is_ancestor"],
        true
    );
    assert_eq!(
        ok(history(
            &f,
            &["is-ancestor", "--ancestor", left, "--head", right]
        ))["is_ancestor"],
        false
    );
    assert!(
        ok(history(
            &f,
            &["merge-bases", "--left", left, "--right", disconnected]
        ))["bases"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // An ancestor is readable via its anchor, but is not itself a registered base.
    let unregistered = &origins[&format!("git:sha1:{a}")];
    for args in [
        vec!["log", "--revision", unregistered],
        vec!["show", "--revision", left, "--commit", disconnected],
        vec!["log", "--revision", left, "--after", disconnected],
        vec!["merge-bases", "--left", left, "--right", unregistered],
    ] {
        assert!(!history(&f, &args).status.success());
    }
    let held_after: i64 = f
        .sql
        .query_one("SELECT count(*) FROM th_object_holds", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        held_before, held_after,
        "failed second root must close first root hold"
    );
    // Same tenant's physical object existence does not grant access through another repository.
    let original_credential = f.credential_path.clone();
    let other = Uuid::new_v4().to_string();
    f.admin_ok(&[
        "repository-create",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &other,
        "--authority",
        "standalone",
    ]);
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &other,
        "--principal",
        "history-reader",
        "--permission",
        "read",
    ]);
    f.credential_path = f.dir.path().join("other-reader.json");
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &other,
        "--principal",
        "history-reader",
        "--permission",
        "read",
        "--output",
        f.credential_path.to_str().unwrap(),
    ]);
    assert!(!history(&f, &["log", "--revision", left]).status.success());
    empty["repository"] = json!(other);
    assert_eq!(native::ok(capture(&f)), empty);
    f.credential_path = original_credential;
    f.sql
        .execute(
            "UPDATE th_repositories SET epoch=2 WHERE tenant=$1 AND repository=$2",
            &[&f.scope.tenant(), &f.scope.repository()],
        )
        .await
        .unwrap();
    assert!(!history(&f, &["log", "--revision", left]).status.success());
    let stale = capture(&f);
    assert!(!stale.status.success());
    let stale: Value = serde_json::from_slice(&stale.stdout).unwrap();
    assert_eq!(stale["error"]["code"], "repository_fenced");
    let args = [
        "history",
        "--authority",
        "standalone",
        "--epoch",
        "2",
        "log",
        "--revision",
        left,
    ];
    assert!(f.cli(&args).status.success());
    f.admin_ok(&[
        "token-revoke",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--token-id",
        issued["token_id"].as_str().unwrap(),
    ]);
    assert!(!f.cli(&args).status.success());
    assert!(!capture(&f).status.success());
    drop(server);
    f.cleanup().await;
}
