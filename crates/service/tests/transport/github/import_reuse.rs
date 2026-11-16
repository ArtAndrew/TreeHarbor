use super::*;
use treeharbor_core::ObjectId;
use treeharbor_service::pack::Pack;

fn open(path: &Path) -> (Pack, Vec<ObjectId>, String) {
    let manifest: Value =
        serde_json::from_slice(&fs::read(path.join("manifest.json")).unwrap()).unwrap();
    let ids = manifest["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().parse().unwrap())
        .collect();
    (
        Pack::open(path).unwrap(),
        ids,
        manifest["source_revision"].as_str().unwrap().to_owned(),
    )
}

fn begin(
    f: &mut Fixture,
    owner: &Path,
    bridge: &Path,
    binding: &Value,
    pack: &Pack,
    revision: &str,
) -> Uuid {
    let import = Uuid::new_v4();
    f.credential_path = owner.to_owned();
    result(request(
        f,
        &json!({"action":"import_request","key":format!("request-{import}"),"epoch":1,
        "import":import,"binding":binding,"specification":{"git_revision":revision,
            "max_objects":100,"max_bytes":1048576,"ttl_seconds":3600}}),
    ));
    f.credential_path = bridge.to_owned();
    result(request(
        f,
        &json!({"action":"import_begin","key":format!("begin-{import}"),"epoch":1,
        "import":import,"native_commit":pack.commit().to_string()}),
    ));
    import
}

async fn send(f: &Fixture, import: Uuid, pack: &Pack, ids: impl IntoIterator<Item = ObjectId>) {
    for id in ids {
        let reply = client::send_github_source_object(
            &f.config.socket,
            &f.credential(),
            import,
            id,
            pack.frame(id).unwrap(),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert!(reply.status.is_success(), "{}", reply.body);
    }
}

fn assert_failure(output: Output, code: &str) {
    assert!(!output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["code"], code, "{response}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; Git bundle and actual bridge import protocol"]
async fn github_import_reuses_only_completed_binding_history() {
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    f.importer();
    let owner = f.credential_path.clone();
    let bridge = provision_bridge(&f);
    let parent_dir = f.dir.path().join("first-pack");
    let child_dir = f.dir.path().join("child-pack");
    git_history_packs(&f, &child_dir, Some(&parent_dir));
    let (parent, parent_ids, parent_git) = open(&parent_dir);
    let (child, child_ids, child_git) = open(&child_dir);
    let base = RevisionRef::Native(parent.commit()).to_string();
    let server = f.start().await;
    let binding: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/github-bind-v1.json"
    ))
    .unwrap();
    result(request(&f, &binding));
    let imported = result(f.cli(&[
        "source-import",
        "--pack",
        parent_dir.to_str().unwrap(),
        "--state",
        f.dir.path().join("ordinary-upload.json").to_str().unwrap(),
        "--authority",
        "connected",
        "--epoch",
        "1",
    ]));
    assert_eq!(imported["revision"], base);

    let child_id = begin(
        &mut f,
        &owner,
        &bridge,
        &binding["binding"],
        &child,
        &child_git,
    );
    let reuse = json!({"action":"import_reuse_base","epoch":1,"import":child_id,"base":base});
    assert_failure(request(&f, &reuse), "not_found");
    f.credential_path = owner.clone();
    assert_failure(request(&f, &reuse), "forbidden");

    let first = begin(
        &mut f,
        &owner,
        &bridge,
        &binding["binding"],
        &parent,
        &parent_git,
    );
    send(&f, first, &parent, parent_ids.iter().copied()).await;
    let parent_finish =
        json!({"action":"import_finish","key":"finish-parent","epoch":1,"import":first});
    assert_eq!(
        result(request(&f, &parent_finish))["git_revision"],
        parent_git
    );
    let mut wrong_epoch = reuse.clone();
    wrong_epoch["epoch"] = json!(2);
    assert_failure(request(&f, &wrong_epoch), "repository_fenced");
    let enrolled = result(request(&f, &reuse));
    assert_eq!(
        enrolled,
        json!({"upload":child_id,"epoch":1,"base":base,"objects":parent_ids.len(),"bytes":parent.encoded_bytes()})
    );
    drop(server);
    let server = f.start().await;
    assert_eq!(result(request(&f, &reuse)), enrolled);
    let child_finish =
        json!({"action":"import_finish","key":"finish-child","epoch":1,"import":child_id});
    assert_failure(request(&f, &child_finish), "upload_membership");
    send(
        &f,
        child_id,
        &child,
        child_ids
            .iter()
            .copied()
            .filter(|id| !parent_ids.contains(id)),
    )
    .await;
    let completed = result(request(&f, &child_finish));
    assert_eq!(completed["git_revision"], child_git);
    assert_eq!(
        completed["revision"],
        RevisionRef::Native(child.commit()).to_string()
    );
    assert_eq!(completed["snapshot"], child.snapshot().to_string());
    assert_eq!(completed["publication_ready"], false);

    // A newer retained graph may contain the requested root. Reuse must not
    // supply that root in place of its direct verified Git-origin frame.
    let reverse = begin(
        &mut f,
        &owner,
        &bridge,
        &binding["binding"],
        &parent,
        &parent_git,
    );
    assert_failure(
        request(
            &f,
            &json!({"action":"import_reuse_base","epoch":1,"import":reverse,
        "base":RevisionRef::Native(child.commit()).to_string()}),
        ),
        "not_found",
    );
    let reserved: i64 = f
        .sql
        .query_one(
            "SELECT reserved_objects FROM th_source_uploads WHERE upload=$1",
            &[&reverse],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(reserved, 0);
    drop(server);
    f.cleanup().await;
}
