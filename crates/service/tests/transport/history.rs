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
#[ignore = "requires PostgreSQL; actual history CLI, Git oracle and credential boundaries"]
async fn history_cli_preserves_criss_cross_pagination_and_scoped_reads_after_restart() {
    let mut f = Fixture::new().await;
    f.importer();
    let repo = f.dir.path().join("oracle.git");
    git_reference::git(
        f.dir.path(),
        &["init", "--bare", "--template=", repo.to_str().unwrap()],
        b"",
    );
    let tree = git_reference::hash(&repo, "tree", b"");
    let root = git_commit(&repo, &tree, &[], "root");
    let a = git_commit(&repo, &tree, &[&root], "a");
    let b = git_commit(&repo, &tree, &[&root], "b");
    let left = git_commit(&repo, &tree, &[&a, &b], "left");
    let right = git_commit(&repo, &tree, &[&b, &a], "right");
    let disconnected = git_commit(&repo, &tree, &[], "independent");
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
    f.credential_path = original_credential;
    f.sql
        .execute(
            "UPDATE th_repositories SET epoch=2 WHERE tenant=$1 AND repository=$2",
            &[&f.scope.tenant(), &f.scope.repository()],
        )
        .await
        .unwrap();
    assert!(!history(&f, &["log", "--revision", left]).status.success());
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
    drop(server);
    f.cleanup().await;
}
