use super::*;
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};
use treeharbor_core::RevisionRef;
use treeharbor_git_bridge::{BundleInput, Limits, import_branches};
use treeharbor_github::ArchivedBranches;
use treeharbor_storage::{LocalStore, TenantId};

#[derive(clap::Args)]
pub struct Options {
    /// Private JSON configuration of the App and pinned repository.
    #[arg(long)]
    config: PathBuf,
    /// Complete branch bundle from this archived repository.
    #[arg(long)]
    bundle: PathBuf,
    /// Existing owner-only directory for temporary Git and native objects.
    #[arg(long)]
    scratch: PathBuf,
    #[arg(long, default_value = "/usr/bin/git")]
    git: PathBuf,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
    timeout_seconds: u64,
}

fn verify_bundle(inventory: ArchivedBranches, options: Options) -> Result<Value> {
    let mut expected = BTreeMap::new();
    for head in &inventory.heads {
        let RevisionRef::Git(oid) = format!("git:sha1:{}", head.target.oid)
            .parse()
            .map_err(|_| Error::Config)?
        else {
            return Err(Error::Config);
        };
        if expected
            .insert(format!("{}{}", head.prefix, head.name), oid)
            .is_some()
        {
            return Err(Error::Config);
        }
    }
    let parent = files::private_directory(&options.scratch)?;
    let stage = tempfile::Builder::new()
        .prefix(".treeharbor-archive-preflight-")
        .tempdir_in(&parent)?;
    fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
    let tenant = TenantId::new("archive-preflight").map_err(crate::pack::Error::Object)?;
    let store = LocalStore::open(stage.path(), &tenant).map_err(crate::pack::Error::Object)?;
    let imported = import_branches(
        &store,
        BundleInput {
            git: &options.git,
            bundle: &options.bundle,
            scratch: stage.path(),
            limits: Limits {
                timeout: Duration::from_secs(options.timeout_seconds),
                ..Limits::default()
            },
        },
        &expected,
    )
    .map_err(crate::pack::Error::Git)?;
    let branches: BTreeMap<_, _> = imported
        .refs
        .into_iter()
        .map(|(name, tip)| {
            (
                name,
                json!({
                    "git_revision":RevisionRef::Git(tip.origin).to_string(),
                    "native_revision":RevisionRef::Native(tip.commit).to_string(),
                    "snapshot":tip.snapshot.to_string(),
                }),
            )
        })
        .collect();
    stage.close()?;
    Ok(json!({
        "version":1,
        "archive":inventory,
        "branches":branches,
        "source_objects":imported.objects.len(),
        "encoded_bytes":imported.encoded_bytes,
    }))
}

pub(super) async fn run(options: Options) -> Result<bool> {
    let (credentials, config) = load(&options.config)?;
    let archive = Connection::new(credentials)?
        .observe_archived_branches(&config.repository)
        .await?;
    let report = tokio::task::spawn_blocking(move || verify_bundle(archive, options))
        .await
        .map_err(|_| Error::ImportWorker)??;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|_| Error::Http)?
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use treeharbor_github::{InventoryHead, InventoryObject};

    #[test]
    fn preflight_maps_the_archived_inventory_to_native_tips_and_removes_scratch() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("repo.git");
        std::fs::create_dir(&source).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("/usr/bin/git")
                .current_dir(&source)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output.stderr);
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "--bare", "--object-format=sha1"]);
        let tree = git(&["mktree"]);
        let commit = git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit-tree",
            &tree,
            "-m",
            "Original commit",
        ]);
        git(&["update-ref", "refs/heads/main", &commit]);
        git(&["update-ref", "refs/heads/topic", &commit]);
        let bundle = root.path().join("source.bundle");
        git(&["bundle", "create", bundle.to_str().unwrap(), "--branches"]);
        let scratch = root.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700)).unwrap();
        let options = || Options {
            config: PathBuf::new(),
            bundle: bundle.clone(),
            scratch: scratch.clone(),
            git: PathBuf::from("/usr/bin/git"),
            timeout_seconds: 30,
        };
        let archive = |topic: &str| ArchivedBranches {
            version: 1,
            binding: RepositoryBinding {
                installation_id: 1,
                repository_id: 2,
                owner_id: 3,
                owner: "example".into(),
                name: "repo".into(),
            },
            app_id: 4,
            repository_node_id: "repo-id".into(),
            default_branch: "main".into(),
            started_at_unix: 10,
            finished_at_unix: 11,
            token_revoked: true,
            heads: [("main", commit.as_str()), ("topic", topic)]
                .into_iter()
                .map(|(name, oid)| InventoryHead {
                    id: name.into(),
                    name: name.into(),
                    prefix: "refs/heads/".into(),
                    target: InventoryObject {
                        kind: "Commit".into(),
                        oid: oid.into(),
                    },
                })
                .collect(),
        };
        let report = verify_bundle(archive(&commit), options()).unwrap();
        assert_eq!(report["version"], 1);
        assert_eq!(report["archive"]["binding"]["repository_id"], 2);
        let main = &report["branches"]["refs/heads/main"];
        assert_eq!(main, &report["branches"]["refs/heads/topic"]);
        assert_eq!(main["git_revision"], format!("git:sha1:{commit}"));
        assert!(matches!(
            main["native_revision"].as_str().unwrap().parse(),
            Ok(RevisionRef::Native(_))
        ));
        assert_eq!(report["source_objects"], 5);
        assert!(matches!(
            verify_bundle(archive(&"b".repeat(40)), options()),
            Err(Error::Pack(crate::pack::Error::Git(
                treeharbor_git_bridge::Error::RefInventoryMismatch
            )))
        ));
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    }
}
