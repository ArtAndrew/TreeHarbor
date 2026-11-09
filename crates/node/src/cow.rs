//! Local, opt-in prepared lower and per-attempt mounted placement.
use crate::{Error, Prepare, Result, private, storage::Operation, worker::clear_unpublished};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};
use treeharbor_core::ObjectId;
use treeharbor_materializer::{
    BaselineExpectation, MaterializedTree, Materializer, Publication, PublicationReceipt, Selection,
};
use treeharbor_metadata::Metadata;
use treeharbor_runtime::{OverlayLayout, OverlayView};

pub(crate) const PIN: &str = "overlay-pin.json";
pub(crate) const RETIRED: &str = "overlay-detach.json";
const ATTACHED: &str = "overlay-attached.json";
const RECORD: &str = "overlay.json";
const MAX_RECORD: usize = 2048;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    version: u8,
    binding: String,
    image: String,
    manifest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attached {
    version: u8,
    pin: String,
    placement: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Retirement {
    version: u8,
    binding: String,
    placement: String,
}

struct ImageGuard {
    _operation: Arc<Operation>,
    _lock: tokio::sync::OwnedMutexGuard<()>,
}

fn digest(bytes: &[u8]) -> String {
    ObjectId::from_bytes(Sha256::digest(bytes).into()).to_string()
}

fn text(bytes: [u8; 32]) -> String {
    ObjectId::from_bytes(bytes).to_string()
}

fn directory(operation: &Operation) -> Result<PathBuf> {
    let parent = private::directory(operation.storage.root.parent().ok_or(Error::Private)?)?;
    Ok(parent.join(format!(
        ".treeharbor-cow-{}",
        operation.storage.identity.storage()
    )))
}

fn layout(operation: &Operation, binding: [u8; 32]) -> Result<OverlayLayout> {
    let dir = directory(operation)?;
    Ok(OverlayLayout {
        lower: dir.join("image/files"),
        upper: operation.root.join("overlay-upper"),
        work: operation.root.join("overlay-work"),
        files: dir.join("views").join(ObjectId::from_bytes(binding).hex()),
    })
}

fn checked_record(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.try_exists()? {
        if private::read(path, MAX_RECORD)? != bytes {
            return Err(Error::Identity);
        }
    } else {
        private::write_new(path, bytes)?;
    }
    Ok(())
}

fn image_key(request: &Prepare<'_>, root: ObjectId) -> Result<[u8; 32]> {
    let plan = request.plan;
    let limits = request.limits;
    let mut hash = Sha256::new();
    hash.update(b"treeharbor-overlay-lower\0v1\0");
    hash.update(serde_json::to_vec(&(
        plan.scope().tenant(),
        plan.scope().repository(),
        plan.authority().as_str(),
        plan.epoch(),
        plan.revision().to_string(),
        plan.snapshot().to_string(),
        plan.profile().id().to_string(),
        plan.git_revision()
            .map(treeharbor_core::RevisionRef::Git)
            .map(|revision| revision.to_string()),
        root.to_string(),
        limits.max_entries,
        limits.max_path_bytes,
        limits.max_file_bytes,
        limits.max_workspace_bytes,
        limits.max_source_bytes,
    ))?);
    Ok(hash.finalize().into())
}

fn invoke(driver: &Path, action: &str, layout: &OverlayLayout) -> Result<()> {
    let metadata = fs::symlink_metadata(driver)?;
    if !driver.is_absolute()
        || !metadata.is_file()
        || ![0, rustix::process::geteuid().as_raw()].contains(&metadata.uid())
        || metadata.mode() & 0o022 != 0
    {
        return Err(Error::Configuration);
    }
    let status = Command::new(driver)
        .arg(action)
        .args([&layout.lower, &layout.upper, &layout.work, &layout.files])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(Error::Incomplete);
    }
    Ok(())
}

pub(crate) async fn prepare(
    db: &mut Metadata,
    request: &Prepare<'_>,
    operation: Arc<Operation>,
    binding: [u8; 32],
    driver: &Path,
    git: Option<&Path>,
) -> Result<MaterializedTree> {
    if operation.volume.is_none() || request.limits.max_workspace_bytes > 256 * 1024 * 1024 {
        return Err(Error::Configuration);
    }
    let plan = request.plan;
    let dir = directory(&operation)?;
    let image = dir.join("image");
    let source = db
        .open_read_session(plan.scope(), plan.source_holder(), 60)
        .await?;
    let key = image_key(request, source.root().id());
    db.close_read_session(source).await?;
    let key = key?;
    let (tree, manifest) = {
        // One bounded image per local storage identity. Its path is never
        // replaced, even after a crash or a request for a different revision.
        let guard = Arc::new(ImageGuard {
            _operation: operation.clone(),
            _lock: operation.storage.cow_image.clone().lock_owned().await,
        });
        operation.verify()?;
        private::make_directory(&dir)?;
        let tree = if image.try_exists()? {
            if git.is_some() {
                let export = db
                    .open_git_preparation_source(request.authorization, plan, 600)
                    .await?;
                db.finish_source_export(export).await?;
            }
            let receipt = PublicationReceipt::load(&image, key)?;
            MaterializedTree::load_baseline(
                &image,
                BaselineExpectation {
                    scope: plan.scope(),
                    snapshot: plan.snapshot(),
                    profile: plan.profile(),
                    manifest_digest: receipt.manifest_digest()?,
                },
                request.limits,
            )?
        } else {
            let staging = dir.join("staging");
            let tenant = plan.scope().tenant();
            clear_unpublished(&operation, &staging, tenant)?;
            let materializer = Materializer::open(&staging, 1)?;
            let publication = Publication::new(image.clone(), key, guard);
            let files = treeharbor_materializer::Preparation {
                scope: plan.scope(),
                holder: plan.source_holder(),
                snapshot: plan.snapshot(),
                selection: Selection::from_profile(plan.profile()),
                limits: request.limits,
            };
            if let Some(git) = git {
                let parent = private::make_directory(&staging.join(tenant))?;
                let source = crate::git_source::GitSource::stage(
                    db,
                    request,
                    operation.clone(),
                    &parent,
                    git,
                )
                .await?;
                materializer
                    .prepare_published_with(db, request.store, files, publication, move |files| {
                        source.seed(files)
                    })
                    .await?
            } else {
                materializer
                    .prepare_published(db, request.store, files, publication)
                    .await?
            }
        };
        let manifest = PublicationReceipt::load(&image, key)?.manifest_digest()?;
        (tree, manifest)
    };
    let driver = driver.to_owned();
    let tenant = plan.scope().tenant().to_owned();
    tokio::task::spawn_blocking(move || {
        operation.verify()?;
        let volume = operation.volume.as_ref().ok_or(Error::Configuration)?;
        let paths = layout(&operation, binding)?;
        private::make_directory(paths.files.parent().ok_or(Error::Private)?)?;
        private::make_directory(&paths.upper)?;
        private::make_directory(&paths.work)?;
        let pin = serde_json::to_vec(&Pin {
            version: 1,
            binding: text(binding),
            image: text(key),
            manifest: text(manifest),
        })?;
        checked_record(&operation.root.join(PIN), &pin)?;
        let attached = operation.root.join(ATTACHED);
        if !OverlayView::mount_present(&paths.files)? {
            if attached.try_exists()? || fs::read_dir(&paths.work)?.next().is_some() {
                return Err(Error::Incomplete);
            }
            private::make_directory(&paths.files)?;
            if fs::read_dir(&paths.files)?.next().is_some() {
                return Err(Error::Incomplete);
            }
            invoke(&driver, "mount", &paths)?;
        }
        if fs::read_dir(&paths.upper)?.next().is_some() {
            return Err(Error::Incomplete);
        }
        let view = OverlayView::observe(volume, paths)?;
        let record = serde_json::to_vec(&Attached {
            version: 1,
            pin: digest(&pin),
            placement: text(view.placement_digest()?),
        })?;
        checked_record(&attached, &record)?;
        let staging = operation.root.join("staging");
        clear_unpublished(&operation, &staging, &tenant)?;
        let parent = private::make_directory(&staging.join(&tenant))?;
        let publication = Publication::new(operation.root.join("tree"), binding, operation.clone());
        let tree = tree.publish_view(
            publication,
            &parent,
            view.files().to_owned(),
            manifest,
            &record,
        )?;
        view.verify()?;
        operation.verify()?;
        Ok(tree)
    })
    .await?
}

pub(crate) fn open(
    operation: &Operation,
    binding: [u8; 32],
    manifest: [u8; 32],
) -> Result<OverlayView> {
    let pin_bytes = private::read(&operation.root.join(PIN), MAX_RECORD)?;
    let pin: Pin = serde_json::from_slice(&pin_bytes)?;
    if pin.version != 1
        || pin.binding != text(binding)
        || pin.manifest != text(manifest)
        || serde_json::to_vec(&pin)? != pin_bytes
    {
        return Err(Error::Identity);
    }
    let root = directory(operation)?;
    let key = *pin
        .image
        .parse::<ObjectId>()
        .map_err(|_| Error::Identity)?
        .as_bytes();
    if PublicationReceipt::load(&root.join("image"), key)?.manifest_digest()? != manifest {
        return Err(Error::Identity);
    }
    let mounted = OverlayView::observe(
        operation.volume.as_ref().ok_or(Error::Identity)?,
        layout(operation, binding)?,
    )?;
    let expected = serde_json::to_vec(&Attached {
        version: 1,
        pin: digest(&pin_bytes),
        placement: text(mounted.placement_digest()?),
    })?;
    if private::read(&operation.root.join(ATTACHED), MAX_RECORD)? != expected
        || private::read(&operation.root.join("tree").join(RECORD), MAX_RECORD)? != expected
    {
        return Err(Error::Identity);
    }
    Ok(mounted)
}

/// Called only after the deletion intent has been admitted and with the slot
/// still exclusively owned. The durable marker survives removal of slot files.
pub(crate) fn detach(
    operation: &Operation,
    driver: &Path,
    journal: &Path,
    binding: [u8; 32],
    manifest: [u8; 32],
) -> Result<()> {
    operation.verify()?;
    let paths = layout(operation, binding)?;
    let marker = journal.join(RETIRED);
    let present = OverlayView::mount_present(&paths.files)?;
    if present {
        let view = open(operation, binding, manifest)?;
        let record = serde_json::to_vec(&Retirement {
            version: 1,
            binding: text(binding),
            placement: text(view.placement_digest()?),
        })?;
        view.verify()?;
        checked_record(&marker, &record)?;
        drop(view);
    } else {
        // A missing mount before cleanup was admitted is an interrupted or
        // replaced placement, never proof that deletion is safe.
        read_retirement(&marker, binding)?;
    }
    if present || paths.work.try_exists()? {
        invoke(driver, "unmount", &paths)?;
    }
    if OverlayView::mount_present(&paths.files)? {
        return Err(Error::Incomplete);
    }
    if paths.files.try_exists()? {
        private::directory(&paths.files)?;
        fs::remove_dir(&paths.files)?;
        fs::File::open(paths.files.parent().ok_or(Error::Private)?)?.sync_all()?;
    }
    operation.verify()
}

fn read_retirement(path: &Path, binding: [u8; 32]) -> Result<[u8; 32]> {
    let bytes = private::read(path, MAX_RECORD)?;
    let record: Retirement = serde_json::from_slice(&bytes)?;
    if record.version != 1
        || record.binding != text(binding)
        || serde_json::to_vec(&record)? != bytes
    {
        return Err(Error::Identity);
    }
    Ok(*record
        .placement
        .parse::<ObjectId>()
        .map_err(|_| Error::Identity)?
        .as_bytes())
}

pub(crate) fn cleanup_context(
    operation: &Operation,
    journal: &Path,
    binding: [u8; 32],
    manifest: [u8; 32],
) -> Result<(PathBuf, [u8; 32])> {
    let files = layout(operation, binding)?.files;
    let marker = journal.join(RETIRED);
    let placement = if marker.try_exists()? {
        read_retirement(&marker, binding)?
    } else {
        open(operation, binding, manifest)?.placement_digest()?
    };
    Ok((files, placement))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_records_match_independent_digest_vectors() {
        let pin = serde_json::to_vec(&Pin {
            version: 1,
            binding: text([0x11; 32]),
            image: text([0x22; 32]),
            manifest: text([0x33; 32]),
        })
        .unwrap();
        assert_eq!(pin.len(), 264);
        assert_eq!(
            digest(&pin),
            "sha256:de3ff4bfbb8880a74c749930bc6d317a51d199bb6b6909431708110bc5bb0046"
        );
        let attached = serde_json::to_vec(&Attached {
            version: 1,
            pin: digest(&pin),
            placement: text([0x44; 32]),
        })
        .unwrap();
        assert_eq!(attached.len(), 179);
        assert_eq!(
            digest(&attached),
            "sha256:f32d06b54c15ae2f0d1abb4b0d96d0f6e49b8827c91faebed8e5abac1a03154a"
        );
        let retirement = serde_json::to_vec(&Retirement {
            version: 1,
            binding: text([0x11; 32]),
            placement: text([0x44; 32]),
        })
        .unwrap();
        assert_eq!(retirement.len(), 183);
        assert_eq!(
            digest(&retirement),
            "sha256:8066d01f1035ce627d7f92b356b71fd746237ca79cf9e020c4f8b2d0497e9ec6"
        );
    }
}
