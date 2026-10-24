use super::*;
use crate::pack::{GitCapture, Pack};
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    sync::Arc,
};
use treeharbor_core::{Object, ObjectStore, RevisionRef};

pub(super) struct Request {
    pub config: PathBuf,
    pub socket: PathBuf,
    pub credential_file: PathBuf,
    pub id: Uuid,
    pub epoch: i64,
    pub state: PathBuf,
    pub git: PathBuf,
}
fn lock(root: &Path) -> Result<File> {
    let path = root.join("import.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    files::private_file(&file)?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| Error::AlreadyRunning)?;
    File::open(root)?.sync_all()?;
    Ok(file)
}
fn bind_intent(root: &Path, value: &Value) -> Result<()> {
    let path = root.join("intent.json");
    match files::write_private(
        &path,
        &serde_json::to_vec(value).map_err(|_| Error::ImportState)?,
    ) {
        Ok(()) => {}
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let actual: Value = serde_json::from_slice(&files::read_private(&path, 16384)?)
        .map_err(|_| Error::ImportState)?;
    if actual != *value {
        return Err(Error::ImportState);
    }
    Ok(())
}
pub(super) async fn run(r: Request) -> Result<bool> {
    let credential = Credential::load(&r.credential_file)?;
    let context = command(
        &r.socket,
        &credential,
        json!({"action":"import_context","epoch":r.epoch,"import":r.id.to_string()}),
    )
    .await?;
    if context["import"] != r.id.to_string() || context["epoch"] != r.epoch {
        return Err(Error::Http);
    }
    if context["state"] == "completed" {
        let result = &context["completed_result"];
        let commit: treeharbor_core::ObjectId = context["native_commit"]
            .as_str()
            .ok_or(Error::Http)?
            .parse()
            .map_err(|_| Error::Http)?;
        let snapshot: treeharbor_core::ObjectId = result["snapshot"]
            .as_str()
            .ok_or(Error::Http)?
            .parse()
            .map_err(|_| Error::Http)?;
        if result["version"] != 1
            || result["import"] != r.id.to_string()
            || result["outcome"] != "completed"
            || result["git_revision"] != context["git_revision"]
            || result["revision"] != RevisionRef::Native(commit).to_string()
            || result["snapshot"] != snapshot.to_string()
            || result["publication_ready"] != false
        {
            return Err(Error::Http);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(result).map_err(|_| Error::Http)?
        );
        return Ok(true);
    }
    if context["unexpired"] != true
        || !matches!(context["state"].as_str(), Some("pending" | "receiving"))
    {
        return Err(Error::ImportState);
    }
    let spec: treeharbor_metadata::github::BindingSpec =
        serde_json::from_value(context["specification"].clone()).map_err(|_| Error::Http)?;
    spec.validate().map_err(|_| Error::Http)?;
    let revision = context["git_revision"]
        .as_str()
        .ok_or(Error::Http)?
        .to_owned();
    let RevisionRef::Git(commit) = revision.parse().map_err(|_| Error::Http)? else {
        return Err(Error::Http);
    };
    let max_objects = context["max_objects"].as_u64().ok_or(Error::Http)?;
    let max_bytes = context["max_bytes"].as_u64().ok_or(Error::Http)?;
    let limits = treeharbor_git_bridge::Limits {
        objects: usize::try_from(max_objects).map_err(|_| Error::Size)?,
        source_bytes: max_bytes,
        stream_bytes: max_bytes.saturating_mul(2).min(128 << 30),
        ..Default::default()
    };
    let root = files::private_directory(&r.state)?;
    let _lock = lock(&root)?;
    bind_intent(
        &root,
        &json!({"version":1,"tenant":credential.scope.tenant(),"repository":credential.scope.repository().to_string(),"import":r.id.to_string(),"epoch":r.epoch,"binding":context["binding"],"specification":spec,"git_revision":revision,"max_objects":max_objects,"max_bytes":max_bytes,"expires_at_unix_ms":context["expires_at_unix_ms"]}),
    )?;
    let (app, config) = load(&r.config)?;
    matches_config(&config, &spec)?;
    let connection = Connection::new(app)?;
    let downloaded = root.join("download");
    if downloaded.try_exists()? {
        // Cached bytes do not replace current App/repository access.
        connection.probe(&config.repository).await?;
    } else {
        connection
            .download_git(
                &config.repository,
                treeharbor_github::GitDownload {
                    worker: std::env::current_exe()?,
                    git: r.git.clone(),
                    output: downloaded.clone(),
                    commit,
                    limits,
                },
            )
            .await?;
    }
    let check_download = downloaded.clone();
    let manifest = tokio::task::spawn_blocking(move || {
        treeharbor_git_bridge::download::open(&check_download, limits)
    })
    .await
    .map_err(|_| Error::ImportWorker)??;
    if manifest.owner != spec.upstream.owner
        || manifest.name != spec.upstream.name
        || manifest.revision != revision
    {
        return Err(Error::ImportState);
    }
    let destination = root.join("pack");
    let git = r.git.clone();
    let pack = tokio::task::spawn_blocking(move || {
        if destination.try_exists()? {
            Pack::open(&destination)
        } else {
            crate::pack::capture_git(GitCapture {
                git: &git,
                bundle: &downloaded.join("history.bundle"),
                commit,
                output: &destination,
                limits,
            })
        }
    })
    .await
    .map_err(|_| Error::ImportWorker)??;
    if pack.object_count() > limits.objects || pack.encoded_bytes() > max_bytes {
        return Err(Error::Size);
    }
    let Object::NativeCommit(native) = pack
        .store()
        .get(pack.commit())
        .map_err(crate::pack::Error::from)?
    else {
        return Err(Error::ImportState);
    };
    if native.git_origin.as_ref().map(|o| o.oid) != Some(commit) {
        return Err(Error::ImportState);
    }
    let result = upload(&r.socket, &credential, r.id, r.epoch, pack).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|_| Error::Http)?
    );
    Ok(true)
}
// Shared by the real bridge and transport fault tests; callers are trusted,
// while server assignment, source membership and every current fence still apply.
pub(crate) async fn upload(
    socket: &Path,
    credential: &Credential,
    id: Uuid,
    epoch: i64,
    pack: Pack,
) -> Result<Value> {
    command(socket,credential,json!({"action":"import_begin","key":format!("github-import-begin-{id}"),"epoch":epoch,"import":id.to_string(),"native_commit":pack.commit().to_string()})).await?;
    let pack = Arc::new(pack);
    let finish = json!({"action":"import_finish","key":format!("github-import-finish-{id}"),"epoch":epoch,"import":id.to_string()});
    for object in &pack.ids {
        let object = *object;
        let reader = pack.clone();
        let frame = tokio::task::spawn_blocking(move || reader.frame(object))
            .await
            .map_err(|_| Error::ImportWorker)??;
        let reply = client::send_github_source_object(
            socket,
            credential,
            id,
            object,
            frame,
            Duration::from_secs(15),
        )
        .await?;
        if !reply.status.is_success() {
            if reply.status.as_u16() == 409 && reply.body["error"]["code"] == "upload_state_changed"
            {
                break;
            }
            return Err(Error::GithubBinding {
                status: reply.status.as_u16(),
            });
        }
        if reply.body["version"] != 1
            || reply.body["result"]["object"] != object.to_string()
            || reply.body["result"]["state"] != "stored"
        {
            return Err(Error::Http);
        }
    }
    let result = command(socket, credential, finish).await?;
    if result["revision"] != RevisionRef::Native(pack.commit()).to_string()
        || result["snapshot"] != pack.snapshot().to_string()
    {
        return Err(Error::Http);
    }
    Ok(result)
}
