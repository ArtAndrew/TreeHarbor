use super::*;
use crate::pack::archive_branches::{self, Destination};
use treeharbor_git_bridge::Limits;
use treeharbor_github::ArchivedBranches;

#[derive(clap::Args)]
pub struct Options {
    /// Private JSON configuration of the App and pinned repository.
    #[arg(long)]
    config: PathBuf,
    /// Complete branch bundle from this archived repository.
    #[arg(long)]
    bundle: PathBuf,
    /// Existing owner-only directory for temporary Git and native objects.
    #[arg(long, required_unless_present = "output", conflicts_with = "output")]
    scratch: Option<PathBuf>,
    /// Save the verified branch history and manifest to a new private directory.
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long, default_value = "/usr/bin/git")]
    git: PathBuf,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
    timeout_seconds: u64,
}

fn verify_bundle(inventory: ArchivedBranches, options: Options) -> Result<Value> {
    let destination = match (&options.scratch, &options.output) {
        (Some(scratch), None) => Destination::Temporary(scratch),
        (None, Some(output)) => Destination::Durable(output),
        _ => return Err(Error::Config),
    };
    Ok(archive_branches::capture(
        inventory,
        &options.git,
        &options.bundle,
        destination,
        Limits {
            timeout: Duration::from_secs(options.timeout_seconds),
            ..Limits::default()
        },
    )?)
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

pub(super) async fn inventory(path: &Path) -> Result<bool> {
    let (credentials, config) = load(path)?;
    let inventory = Connection::new(credentials)?
        .observe_archived_repository(&config.repository)
        .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&inventory).map_err(|_| Error::Http)?
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeSet, fs, os::unix::fs::PermissionsExt, process::Command};
    use treeharbor_core::RevisionRef;
    use treeharbor_git_bridge::HashAlgorithm;
    use treeharbor_github::{InventoryHead, InventoryObject};

    #[test]
    fn archived_spool_round_trips_after_source_loss_and_refuses_corruption() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("repo.git");
        fs::create_dir(&source).unwrap();
        let git = |directory: &Path, args: &[&str]| {
            let output = Command::new("/usr/bin/git")
                .current_dir(directory)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output.stderr);
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&source, &["init", "--bare", "--object-format=sha1"]);
        let tree = git(&source, &["mktree"]);
        let commit = |parents: &[&str], message: &str| {
            let mut args = vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit-tree",
                &tree,
                "-m",
                message,
            ];
            for parent in parents {
                args.extend(["-p", parent]);
            }
            git(&source, &args)
        };
        let base = commit(&[], "Base");
        let right = commit(&[], "Independent second parent");
        let main = commit(&[&base, &right], "Merge");
        git(&source, &["update-ref", "refs/heads/main", &main]);
        git(&source, &["update-ref", "refs/heads/topic", &base]);
        let graph = git(&source, &["rev-list", "--all", "--parents"]);
        let refs = git(
            &source,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads/",
            ],
        );
        let bundle = root.path().join("source.bundle");
        git(
            &source,
            &["bundle", "create", bundle.to_str().unwrap(), "--branches"],
        );
        let scratch = root.path().join("scratch");
        fs::create_dir(&scratch).unwrap();
        fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).unwrap();
        let spool = scratch.join("prepared");
        let options = |output: Option<PathBuf>| Options {
            config: PathBuf::new(),
            bundle: bundle.clone(),
            scratch: output.is_none().then(|| scratch.clone()),
            output,
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
            heads: [("main", main.as_str()), ("topic", topic)]
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
        let report = verify_bundle(archive(&base), options(None)).unwrap();
        assert_eq!(report["version"], 1);
        assert_eq!(report["archive"]["binding"]["repository_id"], 2);
        let main_ref = &report["branches"]["refs/heads/main"];
        assert_eq!(main_ref["git_revision"], format!("git:sha1:{main}"));
        assert_eq!(
            report["branches"]["refs/heads/topic"]["git_revision"],
            format!("git:sha1:{base}")
        );
        assert!(matches!(
            main_ref["native_revision"].as_str().unwrap().parse(),
            Ok(RevisionRef::Native(_))
        ));
        assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
        let rejected = scratch.join("rejected");
        assert!(matches!(
            verify_bundle(archive(&"b".repeat(40)), options(Some(rejected.clone()))),
            Err(Error::Pack(crate::pack::Error::Git(
                treeharbor_git_bridge::Error::RefInventoryMismatch
            )))
        ));
        assert!(!rejected.exists());
        assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
        let retained = verify_bundle(archive(&base), options(Some(spool.clone()))).unwrap();
        assert_eq!(retained["branches"], report["branches"]);
        let fingerprint = retained["spool_fingerprint"].as_str().unwrap();
        let manifest = fs::read(spool.join("manifest.json")).unwrap();
        assert!(matches!(
            verify_bundle(archive(&base), options(Some(spool.clone()))),
            Err(Error::Pack(crate::pack::Error::Output))
        ));
        assert_eq!(fs::read(spool.join("manifest.json")).unwrap(), manifest);
        fs::remove_dir_all(&source).unwrap();
        fs::remove_file(&bundle).unwrap();
        let export = |output: &Path, fingerprint: &str| {
            archive_branches::export(
                &spool,
                Some(fingerprint.parse().unwrap()),
                Path::new("/usr/bin/git"),
                output,
                HashAlgorithm::Sha1,
                Limits::default(),
            )
        };
        let output = scratch.join("git-export");
        assert!(export(&output, &format!("sha256:{}", "9".repeat(64))).is_err());
        assert!(!output.exists());
        export(&output, fingerprint).unwrap();
        let restored = scratch.join("restored.git");
        git(
            &scratch,
            &[
                "clone",
                "--bare",
                output.join("history.bundle").to_str().unwrap(),
                restored.to_str().unwrap(),
            ],
        );
        git(&restored, &["fsck", "--strict"]);
        assert_eq!(
            git(
                &restored,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads/"
                ]
            ),
            refs
        );
        assert_eq!(
            git(&restored, &["rev-list", "--all", "--parents"])
                .lines()
                .map(str::to_owned)
                .collect::<BTreeSet<_>>(),
            graph.lines().map(str::to_owned).collect()
        );
        let exported: Value =
            serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
        let (right_native, _) = exported["commits"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(_, git)| **git == right)
            .unwrap();
        let hex = right_native.strip_prefix("sha256:").unwrap();
        let frame = spool
            .join("objects/archive-preflight")
            .join(&hex[..2])
            .join(&hex[2..]);
        let mut contents = fs::read(&frame).unwrap();
        fs::remove_file(&frame).unwrap();
        assert!(export(&rejected, fingerprint).is_err());
        assert!(!rejected.exists());
        contents[0] ^= 1;
        fs::write(frame, contents).unwrap();
        assert!(export(&rejected, fingerprint).is_err());
        assert!(!rejected.exists());
        assert_eq!(fs::read_dir(&scratch).unwrap().count(), 3);
    }
}
