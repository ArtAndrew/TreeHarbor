use super::*;
use treeharbor_core::ObjectId;

const FORK_PAYLOAD: &[u8] = b"proposal from a fork\x00\xff\n";

pub(super) struct FullArchive {
    bundle: PathBuf,
    pub tag: String,
    pub head: String,
    tag_contents: String,
}

pub(super) fn make_bundle(dir: &Path, repo: &Path, first: &str, tagged: &str) -> FullArchive {
    let blob = git_reference::hash(repo, "blob", FORK_PAYLOAD);
    let tree = String::from_utf8(git_reference::git(
        repo,
        &["mktree"],
        format!("100644 blob {blob}\tpayload.bin\n").as_bytes(),
    ))
    .unwrap();
    let head = git_reference::hash(
        repo,
        "commit",
        format!(
            "tree {}\nparent {first}\nauthor Fixture <fixture@example.invalid> 1234567890 +0000\ncommitter Fixture <fixture@example.invalid> 1234567890 +0000\n\nfork patch\n",
            tree.trim_end()
        )
        .as_bytes(),
    );
    let tag_contents = format!(
        "object {tagged}\ntype commit\ntag v1\ntagger Fixture <fixture@example.invalid> 1234567890 +0000\n\nArchived tag.\n"
    );
    let tag = git_reference::hash(repo, "tag", tag_contents.as_bytes());
    git_reference::git(repo, &["update-ref", "refs/pull/17/head", &head], b"");
    git_reference::git(repo, &["update-ref", "refs/tags/v1", &tag], b"");
    let bundle = dir.join("complete-archive.bundle");
    git_reference::git(
        repo,
        &[
            "bundle",
            "create",
            bundle.to_str().unwrap(),
            "--branches",
            "--tags",
            "--glob=refs/pull/*/head",
        ],
        b"",
    );
    FullArchive {
        bundle,
        tag,
        head,
        tag_contents,
    }
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

pub(super) struct Retained {
    receipt: Value,
    manifest: Vec<u8>,
    bundle: Vec<u8>,
}

pub(super) async fn retain(
    f: &Fixture,
    bridge: &Path,
    observation: &Path,
    archive: &FullArchive,
) -> Retained {
    let dir = f.dir.path();
    let prepared = dir.join("prepared-git-archive");
    success(
        Command::new(&f.binary)
            .args(["github", "archive-preserve", "--state"])
            .arg(observation)
            .arg("--bundle")
            .arg(&archive.bundle)
            .arg("--output")
            .arg(&prepared)
            .output()
            .unwrap(),
    );
    let run = |credential: &Path, journal: &Path| {
        Command::new(&f.binary)
            .env("PATH", "/treeharbor-test-no-executables")
            .args(["github", "archive-retain", "--socket"])
            .arg(&f.config.socket)
            .arg("--credential-file")
            .arg(credential)
            .arg("--archive")
            .arg(&prepared)
            .arg("--state")
            .arg(journal)
            .args(["--epoch", "1"])
            .output()
            .unwrap()
    };
    let denied = run(bridge, &dir.join("archive-bridge.json"));
    assert!(!denied.status.success());
    let denied: Value = serde_json::from_slice(&denied.stdout).unwrap();
    assert_eq!(denied["error"]["code"], "forbidden");
    f.sql
        .batch_execute(
            r#"
        CREATE FUNCTION reject_archive_retention() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.kind='github.archive.retain.v1' THEN
                RAISE EXCEPTION 'injected archive retention failure';
            END IF;
            RETURN NEW;
        END
        $$;
        CREATE TRIGGER reject_archive_retention BEFORE INSERT ON th_operations
        FOR EACH ROW EXECUTE FUNCTION reject_archive_retention();
        "#,
        )
        .await
        .unwrap();
    let journal = dir.join("archive-retention.json");
    let failed = run(&f.credential_path, &journal);
    assert!(!failed.status.success());
    let journal_record: Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    let upload: Uuid = serde_json::from_value(journal_record["upload"].clone()).unwrap();
    let row = f
        .sql
        .query_one(
            r#"
        SELECT state, (SELECT count(*) FROM th_object_pins WHERE holder LIKE 'archive-retained-%')
        FROM th_source_uploads WHERE upload=$1
        "#,
            &[&upload],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, &str>(0), "active");
    assert_eq!(row.get::<_, i64>(1), 0);
    f.sql.batch_execute("DROP TRIGGER reject_archive_retention ON th_operations; DROP FUNCTION reject_archive_retention();").await.unwrap();
    let result = success(run(&f.credential_path, &journal));
    let receipt = &result["result"];
    assert_eq!(receipt["state"], "retained");
    assert_eq!(receipt["upload"], json!(upload));
    f.sql
        .execute(
            "UPDATE th_source_uploads SET expires_at=clock_timestamp() WHERE upload=$1",
            &[&upload],
        )
        .await
        .unwrap();
    assert_eq!(success(run(&f.credential_path, &journal)), result);
    let retained = Retained {
        receipt: receipt.clone(),
        manifest: fs::read(prepared.join("manifest.json")).unwrap(),
        bundle: fs::read(prepared.join("archive.bundle")).unwrap(),
    };
    fs::remove_dir_all(prepared).unwrap();
    fs::remove_file(&archive.bundle).unwrap();
    fs::remove_file(journal).unwrap();
    retained
}

pub(super) async fn recover(f: &Fixture, bridge: &Path, retained: Retained, archive: &FullArchive) {
    let key = retained.receipt["key"].as_str().unwrap();
    let output = f.dir.path().join("recovered-git-archive");
    let recover = |config: &Path, credential: &Path, output: &Path| {
        Command::new(&f.binary)
            .env("PATH", "/treeharbor-test-no-executables")
            .args(["github", "archive-recover", "--config"])
            .arg(config)
            .arg("--credential-file")
            .arg(credential)
            .args(["--key", key, "--output"])
            .arg(output)
            .output()
            .unwrap()
    };
    assert!(!recover(&f.config_path, bridge, &output).status.success());
    assert!(!output.exists());
    let upload: Uuid = serde_json::from_value(retained.receipt["upload"].clone()).unwrap();
    let row = f.sql.query_one(
        "SELECT digest,generation FROM th_source_upload_items WHERE upload=$1 AND kind=1 ORDER BY digest LIMIT 1",
        &[&upload],
    ).await.unwrap();
    let digest = ObjectId::from_bytes(row.get::<_, Vec<u8>>(0).try_into().unwrap()).hex();
    let storage = f.config.storage.as_ref().unwrap();
    let frame = storage
        .root
        .join("managed-v1")
        .join(row.get::<_, i64>(1).to_string())
        .join("objects")
        .join(f.scope.tenant())
        .join(&digest[..2])
        .join(&digest[2..]);
    let withheld = f.dir.path().join("withheld-archive-payload");
    fs::rename(&frame, &withheld).unwrap();
    assert!(
        !recover(&f.config_path, &f.credential_path, &output)
            .status
            .success()
    );
    assert!(!output.exists());
    fs::rename(&withheld, frame).unwrap();
    let result = success(recover(&f.config_path, &f.credential_path, &output));
    assert_eq!(result["state"], "recovered");
    assert_eq!(
        fs::read(output.join("manifest.json")).unwrap(),
        retained.manifest
    );
    let bundle = output.join("archive.bundle");
    assert_eq!(fs::read(&bundle).unwrap(), retained.bundle);
    let restored = f.dir.path().join("recovered-git-repo");
    fs::create_dir(&restored).unwrap();
    git_reference::git(
        &restored,
        &["init", "--bare", "--template=", "--object-format=sha1"],
        b"",
    );
    git_reference::git(
        &restored,
        &["bundle", "verify", bundle.to_str().unwrap()],
        b"",
    );
    git_reference::git(
        &restored,
        &[
            "fetch",
            "--no-tags",
            bundle.to_str().unwrap(),
            "refs/heads/*:refs/heads/*",
            "refs/tags/*:refs/tags/*",
            "refs/pull/*:refs/pull/*",
        ],
        b"",
    );
    git_reference::git(&restored, &["fsck", "--strict"], b"");
    assert_eq!(
        git_reference::git(
            &restored,
            &["cat-file", "blob", "refs/pull/17/head:payload.bin"],
            b""
        ),
        FORK_PAYLOAD
    );
    assert_eq!(
        git_reference::git(&restored, &["cat-file", "tag", &archive.tag], b""),
        archive.tag_contents.as_bytes()
    );
    assert!(
        !recover(&f.config_path, &f.credential_path, &output)
            .status
            .success()
    );

    let dir = f.dir.path();
    let backup = dir.join("archive-backup");
    let receipt = success(
        Command::new(&f.binary)
            .args(["backup", "create", "--writers-stopped", "--config"])
            .arg(&f.config_path)
            .arg("--output")
            .arg(&backup)
            .arg("--pg-dump")
            .arg(std::env::var_os("TH_TEST_PG_DUMP").unwrap())
            .arg("--state")
            .arg(&f.credential_path)
            .output()
            .unwrap(),
    );
    let digest = receipt["manifest_sha256"].as_str().unwrap();
    let stage = dir.join("archive-restore");
    let url_file = dir.join("archive-restore.url");
    files::write_private(&url_file, f.config.database_url.as_bytes()).unwrap();
    let staged = success(
        Command::new(&f.binary)
            .args(["backup", "restore-stage", "--isolated-target", "--backup"])
            .arg(&backup)
            .arg("--directory")
            .arg(&stage)
            .args(["--manifest-sha256", digest, "--database-url-file"])
            .arg(&url_file)
            .arg("--pg-restore")
            .arg(std::env::var_os("TH_TEST_PG_RESTORE").unwrap())
            .output()
            .unwrap(),
    );
    let target = staged["database"].as_str().unwrap();
    assert!(target.starts_with("th_restore_") && target.len() == 43);
    let stage_command = |action: &str| {
        let mut command = Command::new(&f.binary);
        command
            .args(["backup", action, "--writers-stopped", "--stage"])
            .arg(&stage)
            .args(["--manifest-sha256", digest]);
        command
    };
    success(stage_command("restore-quarantine").output().unwrap());
    let configured = success(
        stage_command("restore-configure")
            .arg("--old-infrastructure-fenced")
            .output()
            .unwrap(),
    );
    let config = Path::new(configured["config"].as_str().unwrap());
    fs::rename(&storage.root, dir.join("old-storage-unavailable")).unwrap();
    fs::remove_dir_all(output).unwrap();
    let from_backup = dir.join("archive-from-backup");
    assert!(
        !recover(config, &f.credential_path, &from_backup)
            .status
            .success()
    );
    assert!(!from_backup.exists());
    let restored_credential = dir.join("restored-archive-owner.json");
    success(
        Command::new(&f.binary)
            .args(["admin", "--config"])
            .arg(config)
            .args([
                "token-issue",
                "--tenant",
                f.scope.tenant(),
                "--repository",
                &f.scope.repository().to_string(),
                "--principal",
                "importer",
                "--permission",
                "repository.write",
                "--output",
            ])
            .arg(&restored_credential)
            .output()
            .unwrap(),
    );
    assert_eq!(
        success(recover(config, &restored_credential, &from_backup))["state"],
        "recovered"
    );
    assert_eq!(
        fs::read(from_backup.join("manifest.json")).unwrap(),
        retained.manifest
    );
    assert_eq!(
        fs::read(from_backup.join("archive.bundle")).unwrap(),
        retained.bundle
    );
    f.sql
        .batch_execute(&format!("DROP DATABASE {target} WITH (FORCE)"))
        .await
        .unwrap();
}
