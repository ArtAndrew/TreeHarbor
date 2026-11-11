use super::{Error, Limits, MAX_PACK_BYTES, MAX_PACK_OBJECTS, Pack};
use crate::{Error as ServiceError, archive, client, files};
use serde_json::json;
use std::{
    collections::{BTreeMap, btree_map::Entry},
    fs,
    io::ErrorKind,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use treeharbor_core::{Object, ObjectId, ObjectStore, RevisionRef};
use treeharbor_git_bridge as git_bridge;
use treeharbor_metadata::{Authority, exports::ExportFence};
use treeharbor_storage::LocalStore;

/// Read only the verified members of the input packs. Shared objects use one
/// locator. An invalid pack cannot borrow missing members from another pack.
#[derive(Default)]
struct PackObjects {
    stores: Vec<LocalStore>,
    locations: BTreeMap<ObjectId, usize>,
}

impl ObjectStore for PackObjects {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        let store = self
            .locations
            .get(&id)
            .ok_or(treeharbor_core::Error::MissingObject(id))?;
        self.stores[*store].get(id)
    }

    fn put(&self, _: &Object) -> treeharbor_core::Result<ObjectId> {
        Err(treeharbor_core::Error::InvalidObject(
            "read-only export packs",
        ))
    }
}

pub(crate) fn export(
    branches: BTreeMap<String, PathBuf>,
    git: &Path,
    output: &Path,
    algorithm: git_bridge::HashAlgorithm,
    mut limits: git_bridge::Limits,
) -> crate::Result<git_bridge::Manifest> {
    if branches.is_empty() || branches.len() > git_bridge::MAX_EXPORT_REFS {
        return Err(git_bridge::Error::Configuration.into());
    }
    let deadline = Instant::now() + limits.timeout;
    let mut input_budget = Limits {
        objects: MAX_PACK_OBJECTS,
        bytes: MAX_PACK_BYTES,
    };
    let mut objects = PackObjects::default();
    let mut tips = BTreeMap::new();
    let mut refs = BTreeMap::new();
    for (name, path) in branches {
        if Instant::now() >= deadline {
            return Err(git_bridge::Error::Limit.into());
        }
        let directory = crate::files::private_directory(&path).map_err(|_| Error::Output)?;
        let tip = match tips.entry(directory) {
            Entry::Occupied(tip) => *tip.get(),
            Entry::Vacant(tip) => {
                let pack = Pack::open_private(tip.key().clone(), input_budget, Some(deadline))?;
                input_budget.objects -= pack.ids.len() as u64;
                input_budget.bytes -= pack.bytes;
                let store = objects.stores.len();
                for id in pack.ids {
                    objects.locations.entry(id).or_insert(store);
                }
                objects.stores.push(pack.store);
                *tip.insert(pack.commit)
            }
        };
        refs.insert(name, tip);
    }
    limits.timeout = deadline.saturating_duration_since(Instant::now());
    if limits.timeout.is_zero() {
        return Err(git_bridge::Error::Limit.into());
    }
    Ok(git_bridge::export(
        &objects,
        git_bridge::Export {
            git,
            output,
            refs,
            algorithm,
            limits,
        },
    )?)
}

pub(crate) struct LiveExport<'a> {
    pub socket: &'a Path,
    pub credential: &'a client::Credential,
    pub epoch: i64,
    pub output: PathBuf,
    pub git: PathBuf,
    pub algorithm: git_bridge::HashAlgorithm,
    pub request_timeout: Duration,
    pub timeout: Duration,
}

fn remaining(deadline: Instant) -> crate::Result<Duration> {
    let duration = deadline.saturating_duration_since(Instant::now());
    if duration.is_zero() {
        return Err(git_bridge::Error::Limit.into());
    }
    Ok(duration)
}

pub(crate) async fn export_standalone(request: LiveExport<'_>) -> crate::Result<client::Reply> {
    let deadline = Instant::now() + request.timeout;
    let parent = files::private_directory(request.output.parent().ok_or(Error::Output)?)?;
    let output = parent.join(request.output.file_name().ok_or(Error::Output)?);
    match output.symlink_metadata() {
        Ok(_) => return Err(Error::Output.into()),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let body = request
        .credential
        .request(json!({"method":"native","request":{"action":"ref_snapshot"}}));
    let mut reply = client::send(
        request.socket,
        &request.credential.token,
        serde_json::to_vec(&body).map_err(|_| ServiceError::Config)?,
        request.request_timeout.min(remaining(deadline)?),
    )
    .await?;
    if !reply.status.is_success() {
        return Ok(reply);
    }

    let snapshot = reply.body["result"].take();
    let scope = &request.credential.scope;
    if snapshot["version"] != 1
        || snapshot["tenant"] != scope.tenant()
        || snapshot["repository"] != scope.repository().to_string()
        || snapshot["authority"] != Authority::Standalone.as_str()
        || snapshot["epoch"] != request.epoch
    {
        return Err(ServiceError::ExportSnapshot);
    }
    let captured = snapshot["refs"]
        .as_array()
        .filter(|refs| !refs.is_empty() && refs.len() <= git_bridge::MAX_EXPORT_REFS)
        .ok_or(ServiceError::ExportSnapshot)?;
    let mut branches = BTreeMap::new();
    let mut heads = BTreeMap::new();
    for reference in captured {
        let field = |name: &str| reference[name].as_str().ok_or(ServiceError::ExportSnapshot);
        let name = field("name")?;
        let RevisionRef::Native(head) = field("head")?
            .parse()
            .map_err(|_| ServiceError::ExportSnapshot)?
        else {
            return Err(ServiceError::ExportSnapshot);
        };
        let tree: ObjectId = field("snapshot")?
            .parse()
            .map_err(|_| ServiceError::ExportSnapshot)?;
        if name.is_empty()
            || reference["epoch"] != request.epoch
            || branches
                .insert(format!("refs/heads/{name}"), head)
                .is_some()
            || heads.insert(head, tree).is_some_and(|prior| prior != tree)
        {
            return Err(ServiceError::ExportSnapshot);
        }
    }

    let stage = tempfile::Builder::new()
        .prefix(".treeharbor-branch-packs-")
        .tempdir_in(&parent)?;
    fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
    let mut bytes = MAX_PACK_BYTES;
    let mut objects = MAX_PACK_OBJECTS;
    for (revision, tree) in heads {
        let path = stage.path().join(super::hex(revision.as_bytes()));
        let downloaded = archive::download(
            request.socket,
            request.credential,
            ExportFence {
                authority: Authority::Standalone,
                epoch: request.epoch,
                revision,
            },
            &path,
            request.request_timeout.min(remaining(deadline)?),
        )
        .await?;
        if !downloaded.status.is_success() {
            return Ok(downloaded);
        }
        if downloaded.body["snapshot"] != tree.to_string() {
            return Err(ServiceError::ExportSnapshot);
        }
        bytes = bytes
            .checked_sub(
                downloaded.body["encoded_bytes"]
                    .as_u64()
                    .ok_or(Error::Invalid)?,
            )
            .ok_or(Error::Limit)?;
        objects = objects
            .checked_sub(downloaded.body["objects"].as_u64().ok_or(Error::Invalid)?)
            .ok_or(Error::Limit)?;
    }
    let paths = branches
        .into_iter()
        .map(|(name, revision)| (name, stage.path().join(super::hex(revision.as_bytes()))))
        .collect();
    let git = request.git;
    let algorithm = request.algorithm;
    let manifest = tokio::task::spawn_blocking(move || {
        let result = export(
            paths,
            &git,
            &output,
            algorithm,
            git_bridge::Limits {
                timeout: remaining(deadline)?,
                ..Default::default()
            },
        );
        // The projection reads pack files throughout the blocking operation.
        drop(stage);
        result
    })
    .await
    .map_err(|_| ServiceError::ExportWorker)??;
    Ok(client::Reply {
        status: reply.status,
        body: json!({"snapshot":snapshot,"export":manifest}),
    })
}
