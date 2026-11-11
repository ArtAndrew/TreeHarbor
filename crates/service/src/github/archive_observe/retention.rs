use super::{Credential, Error, Result, Uuid, files, json};
use crate::{
    config::Config,
    database::Database,
    import::{Finish, Transfer, retain_journal},
};
use rustix::fs::OFlags;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use treeharbor_metadata::{
    Scope,
    access::Permission,
    commands::MAX_REQUEST_BYTES,
    github::{ArchiveArtifact, ArchiveArtifactSpec, pack_archive_artifact},
};
use treeharbor_storage::{LocalStore, ManagedStore, TenantId};

#[derive(clap::Args)]
pub struct RetainOptions {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    credential_file: PathBuf,
    /// Directory produced by archive-preserve.
    #[arg(long)]
    archive: PathBuf,
    /// Private persistent upload journal, retained on retry.
    #[arg(long)]
    state: PathBuf,
    #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
    epoch: i64,
    #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(i32).range(1..=3600))]
    ttl_seconds: i32,
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=3600))]
    request_timeout_seconds: u64,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Intent {
    tenant: String,
    repository: Uuid,
    epoch: i64,
    ttl_seconds: i32,
    specification: ArchiveArtifactSpec,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    upload: Uuid,
    intent: Intent,
}

fn pack(path: &Path, scratch: &Path) -> Result<(tempfile::TempDir, LocalStore, ArchiveArtifact)> {
    let path = files::private_directory(path)?;
    let manifest = files::read_private(&path.join("manifest.json"), treeharbor_core::MAX_PAYLOAD)?;
    let bundle = OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
        .open(path.join("archive.bundle"))?;
    files::private_file(&bundle)?;
    let temp = tempfile::Builder::new()
        .prefix(".treeharbor-archive-upload-")
        .tempdir_in(scratch)?;
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))?;
    let tenant = TenantId::new("archive-artifact").map_err(crate::pack::Error::Object)?;
    let store = LocalStore::open(temp.path(), &tenant).map_err(crate::pack::Error::Object)?;
    let artifact = pack_archive_artifact(
        &store,
        manifest,
        bundle,
        Instant::now() + Duration::from_secs(3600),
    )
    .map_err(Error::ArchiveArtifact)?;
    Ok((temp, store, artifact))
}

pub(in crate::github) async fn retain(options: RetainOptions) -> Result<bool> {
    let credential = Credential::load(&options.credential_file)?;
    let scratch = files::private_directory(options.state.parent().ok_or(Error::UnsafeFile)?)?;
    let (_temp, store, artifact) =
        tokio::task::spawn_blocking(move || pack(&options.archive, &scratch))
            .await
            .map_err(|_| Error::ImportWorker)??;
    let proposed = Journal {
        version: 1,
        upload: Uuid::new_v4(),
        intent: Intent {
            tenant: credential.scope.tenant().into(),
            repository: credential.scope.repository(),
            epoch: options.epoch,
            ttl_seconds: options.ttl_seconds,
            specification: artifact.specification,
        },
    };
    let journal: Journal = retain_journal(&options.state, &proposed, MAX_REQUEST_BYTES)?;
    if journal.version != 1 || journal.upload.is_nil() || journal.intent != proposed.intent {
        return Err(Error::ImportState);
    }
    let intent = journal.intent;
    let upload = journal.upload;
    let spec = intent.specification;
    let transfer = Transfer {
        socket: &options.socket,
        credential: &credential,
        timeout: Duration::from_secs(options.request_timeout_seconds),
        upload,
        begin: json!({"method":"source_begin","key":format!("archive-payload-{upload}-begin"),
            "authority":"connected","epoch":intent.epoch,"upload":upload,"ttl_seconds":intent.ttl_seconds,
            "max_objects":spec.object_count,"max_bytes":spec.encoded_bytes}),
        finish: Finish::ArchiveArtifact(
            json!({"epoch":intent.epoch,"upload":upload,"specification":spec}),
        ),
    };
    let reply = transfer.send(artifact.objects, store).await?;
    if reply.status.is_success() {
        let suffix = spec
            .observation_key
            .strip_prefix("archive-observed-")
            .ok_or(Error::ImportState)?;
        let result = &reply.body["result"];
        if reply.body["version"] != 1
            || result["version"] != 1
            || result["state"] != "retained"
            || result["key"] != format!("archive-retained-{suffix}")
            || result["tenant"] != intent.tenant
            || result["repository"] != json!(intent.repository)
            || result["epoch"] != intent.epoch
            || result["upload"] != json!(upload)
            || result["specification"] != json!(spec)
        {
            return Err(Error::Http);
        }
    }
    println!("{}", reply.body);
    Ok(reply.status.is_success())
}

#[derive(clap::Args)]
pub struct RecoverOptions {
    /// Private config for the service or restored metadata and object store.
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    credential_file: PathBuf,
    /// Exact retained archive receipt key.
    #[arg(long)]
    key: String,
    /// New private output directory.
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
    timeout_seconds: u64,
}

pub(in crate::github) async fn recover(options: RecoverOptions) -> Result<bool> {
    let config = Config::load(&options.config)?;
    let storage = config.storage.clone().ok_or(Error::Config)?;
    let credential = Credential::load(&options.credential_file)?;
    let scope: Scope = credential.scope.clone();
    let mut db = Database::new(config)?.connect(true).await?;
    let auth = db
        .metadata
        .authorize(&credential.token, &scope, Permission::RepositoryWrite)
        .await
        .map_err(Error::ArchiveArtifact)?;
    let archive = db
        .metadata
        .open_retained_archive(&auth, &options.key)
        .await
        .map_err(Error::ArchiveArtifact)?;
    let parent = files::private_directory(options.output.parent().ok_or(Error::UnsafeFile)?)?;
    let name = options
        .output
        .file_name()
        .ok_or(Error::UnsafeFile)?
        .to_owned();
    if parent.join(&name).symlink_metadata().is_ok() {
        return Err(Error::Io(std::io::Error::from(
            std::io::ErrorKind::AlreadyExists,
        )));
    }
    let stage = tempfile::Builder::new()
        .prefix(".treeharbor-archive-recover-")
        .tempdir_in(&parent)?;
    fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
    let tenant = TenantId::new(scope.tenant()).map_err(crate::pack::Error::Object)?;
    let deadline = Instant::now() + Duration::from_secs(options.timeout_seconds);
    let stage = tokio::task::spawn_blocking(move || {
        let store = ManagedStore::open(storage.root, tenant, &storage.backend)
            .map_err(crate::pack::Error::Object)?;
        let mut bundle = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(stage.path().join("archive.bundle"))?;
        let manifest = archive
            .write(&store, deadline, &mut bundle)
            .map_err(Error::ArchiveArtifact)?;
        bundle.sync_all()?;
        files::write_private(&stage.path().join("manifest.json"), &manifest)?;
        Ok::<_, Error>(stage)
    })
    .await
    .map_err(|_| Error::ExportWorker)??;
    db.metadata
        .authorize(&credential.token, &scope, Permission::RepositoryWrite)
        .await
        .map_err(Error::ArchiveArtifact)?;
    crate::pack::publish_stage(stage.path(), &parent, &name)?;
    println!(
        "{}",
        json!({"version":1,"state":"recovered","key":options.key})
    );
    Ok(true)
}
