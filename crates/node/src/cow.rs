//! Local, opt-in prepared lower and per-attempt mounted placement.
use crate::{
    Error, Prepare, Result, cow_volume, private,
    storage::{Operation, Storage},
    worker::clear_unpublished,
};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<String>,
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

pub(crate) fn directory(storage: &Storage) -> Result<PathBuf> {
    let parent = private::directory(storage.root.parent().ok_or(Error::Private)?)?;
    Ok(parent.join(format!(".treeharbor-cow-{}", storage.identity.storage())))
}

fn image_path(
    source: &Path,
    max_images: Option<u8>,
    key: Option<ObjectId>,
    reserve: bool,
) -> Result<PathBuf> {
    let Some(max_images) = max_images else {
        return Ok(source.join("image"));
    };
    let cache = source.join("images");
    let actual = if reserve {
        private::make_directory(&cache)?
    } else {
        private::directory(&cache)?
    };
    let device = fs::metadata(source)?.dev();
    if actual != cache || fs::metadata(&cache)?.dev() != device {
        return Err(Error::Identity);
    }
    let entry = cache.join(key.ok_or(Error::Identity)?.hex());
    let actual = if reserve && !entry.try_exists()? {
        for (index, item) in fs::read_dir(&cache)?.enumerate() {
            item?;
            if index + 1 >= usize::from(max_images) {
                return Err(Error::ImageLimit);
            }
        }
        private::make_directory(&entry)?
    } else {
        private::directory(&entry)?
    };
    // The dedicated volume and both path components must still be private and
    // resolve to the registered path, even when reopening an old placement.
    if actual != entry || fs::metadata(&entry)?.dev() != device {
        return Err(Error::Identity);
    }
    Ok(entry.join("image"))
}

fn source_image(storage: &Storage, key: Option<ObjectId>) -> Result<(PathBuf, bool)> {
    match cow_volume::open(storage)? {
        Some(source) => Ok((
            image_path(source.volume.files(), source.max_images, key, false)?,
            source.max_images.is_some(),
        )),
        None => Ok((directory(storage)?.join("image"), false)),
    }
}

fn view_path(operation: &Operation, binding: [u8; 32]) -> Result<PathBuf> {
    Ok(directory(&operation.storage)?
        .join("views")
        .join(ObjectId::from_bytes(binding).hex()))
}

fn layout(operation: &Operation, binding: [u8; 32], image: &Path) -> Result<OverlayLayout> {
    Ok(OverlayLayout {
        lower: image.join("files"),
        upper: operation.root.join("overlay-upper"),
        work: operation.root.join("overlay-work"),
        files: view_path(operation, binding)?,
    })
}

fn checked_record(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.try_exists()? {
        if private::read(path, MAX_RECORD)? != bytes {
            return Err(Error::Identity);
        }
        fs::File::open(path.parent().ok_or(Error::Private)?)?.sync_all()?;
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
    git: Option<crate::GitPreparation<'_>>,
) -> Result<MaterializedTree> {
    if operation.volume.is_none() || request.limits.max_workspace_bytes > 256 * 1024 * 1024 {
        return Err(Error::Configuration);
    }
    let plan = request.plan;
    let dir = directory(&operation.storage)?;
    let (image, key, tree, manifest) = {
        // Serialize source registration and publication. Neither a published
        // image nor the lower path of an existing view is ever replaced.
        let guard = Arc::new(ImageGuard {
            _operation: operation.clone(),
            _lock: operation.storage.cow_image.clone().lock_owned().await,
        });
        operation.verify()?;
        // Registration uses this same image lock. Select its directory only
        // after taking the lock, so the first build cannot race registration.
        let source_volume = cow_volume::open(&operation.storage)?;
        let source_dir = source_volume
            .as_ref()
            .map_or(dir.as_path(), |source| source.volume.files());
        let max_images = source_volume.as_ref().and_then(|source| source.max_images);
        private::make_directory(&dir)?;
        let root =
            if git.is_some() && (max_images.is_some() || source_dir.join("image").try_exists()?) {
                db.cached_git_preparation_root(request.authorization, plan)
                    .await?
            } else {
                let source = db
                    .open_read_session(plan.scope(), plan.source_holder(), 60)
                    .await?;
                let root = source.root().id();
                db.close_read_session(source).await?;
                root
            };
        let key = image_key(request, root)?;
        let image = image_path(
            source_dir,
            max_images,
            Some(ObjectId::from_bytes(key)),
            true,
        )?;
        if image.try_exists()? {
            let manifest = PublicationReceipt::load(&image, key)?.manifest_digest()?;
            let tree = MaterializedTree::load_baseline(
                &image,
                BaselineExpectation {
                    scope: plan.scope(),
                    snapshot: plan.snapshot(),
                    profile: plan.profile(),
                    manifest_digest: manifest,
                },
                request.limits,
            )?;
            (image, key, tree, manifest)
        } else {
            let staging = source_dir.join("staging");
            let tenant = plan.scope().tenant();
            clear_unpublished(&operation, &staging, tenant)?;
            let materializer = Materializer::open(&staging, 1)?;
            let mut publication = Publication::new(image.clone(), key, guard);
            if let Some(source) = &source_volume
                && let Some(fd) = source.volume.preparation_sync_fd()?
            {
                publication = publication.with_volume_sync(fd);
            }
            let files = treeharbor_materializer::Preparation {
                scope: plan.scope(),
                holder: plan.source_holder(),
                snapshot: plan.snapshot(),
                selection: Selection::from_profile(plan.profile()),
                limits: request.limits,
            };
            let tree = if let Some(git) = git {
                // GitSource may still be loading a seed after cancellation.
                // Keep its scratch under the slot guard that it already owns.
                let scratch = operation.root.join("staging");
                clear_unpublished(&operation, &scratch, tenant)?;
                let parent = private::make_directory(&scratch.join(tenant))?;
                let parent = private::make_directory(&parent.join("prepare-git-source"))?;
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
            };
            let manifest = PublicationReceipt::load(&image, key)?.manifest_digest()?;
            (image, key, tree, manifest)
        }
    };
    let driver = driver.to_owned();
    let tenant = plan.scope().tenant().to_owned();
    tokio::task::spawn_blocking(move || {
        operation.verify()?;
        let volume = operation.volume.as_ref().ok_or(Error::Configuration)?;
        // The dedicated source volume could have disappeared between image
        // publication and attachment. Recheck its registered identity here.
        if source_image(&operation.storage, Some(ObjectId::from_bytes(key)))?.0 != image {
            return Err(Error::Identity);
        }
        let paths = layout(&operation, binding, &image)?;
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
        }
        // The packaged driver also reconciles an interrupted propagation change
        // on the exact existing overlay. It never replaces a mismatched mount.
        invoke(&driver, "mount", &paths)?;
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
    let (paths, pin, _) = pinned_layout(operation, binding, manifest)?;
    open_attached(operation, paths, &pin)
}

fn pinned_layout(
    operation: &Operation,
    binding: [u8; 32],
    manifest: [u8; 32],
) -> Result<(OverlayLayout, Vec<u8>, Option<ObjectId>)> {
    let pin_bytes = private::read(&operation.root.join(PIN), MAX_RECORD)?;
    let pin: Pin = serde_json::from_slice(&pin_bytes)?;
    if pin.version != 1
        || pin.binding != text(binding)
        || pin.manifest != text(manifest)
        || serde_json::to_vec(&pin)? != pin_bytes
    {
        return Err(Error::Identity);
    }
    let key = pin.image.parse::<ObjectId>().map_err(|_| Error::Identity)?;
    let (image, multi) = source_image(&operation.storage, Some(key))?;
    if PublicationReceipt::load(&image, *key.as_bytes())?.manifest_digest()? != manifest {
        return Err(Error::Identity);
    }
    Ok((
        layout(operation, binding, &image)?,
        pin_bytes,
        multi.then_some(key),
    ))
}

fn open_attached(operation: &Operation, paths: OverlayLayout, pin: &[u8]) -> Result<OverlayView> {
    let mounted = OverlayView::observe(operation.volume.as_ref().ok_or(Error::Identity)?, paths)?;
    let expected = serde_json::to_vec(&Attached {
        version: 1,
        pin: digest(pin),
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
    let files = view_path(operation, binding)?;
    let marker = journal.join(RETIRED);
    if OverlayView::mount_present(&files)? {
        let (paths, pin, image) = pinned_layout(operation, binding, manifest)?;
        let view = open_attached(operation, paths.clone(), &pin)?;
        let record = serde_json::to_vec(&Retirement {
            version: if image.is_some() { 2 } else { 1 },
            binding: text(binding),
            placement: text(view.placement_digest()?),
            image: image.map(|key| key.to_string()),
        })?;
        view.verify()?;
        checked_record(&marker, &record)?;
        drop(view);
        invoke(driver, "unmount", &paths)?;
    } else {
        // A missing mount before cleanup was admitted is an interrupted or
        // replaced placement, never proof that deletion is safe.
        let (_, key) = read_retirement(&marker, binding)?;
        let (image, multi) = source_image(&operation.storage, key)?;
        if multi != key.is_some() {
            return Err(Error::Identity);
        }
        if operation.root.join("overlay-work").try_exists()? {
            invoke(driver, "unmount", &layout(operation, binding, &image)?)?;
        }
    }
    if OverlayView::mount_present(&files)? {
        return Err(Error::Incomplete);
    }
    if files.try_exists()? {
        private::directory(&files)?;
        fs::remove_dir(&files)?;
        fs::File::open(files.parent().ok_or(Error::Private)?)?.sync_all()?;
    }
    operation.verify()
}

fn read_retirement(path: &Path, binding: [u8; 32]) -> Result<([u8; 32], Option<ObjectId>)> {
    let bytes = private::read(path, MAX_RECORD)?;
    let record: Retirement = serde_json::from_slice(&bytes)?;
    if !matches!((record.version, &record.image), (1, None) | (2, Some(_)))
        || record.binding != text(binding)
        || serde_json::to_vec(&record)? != bytes
    {
        return Err(Error::Identity);
    }
    let placement = *record
        .placement
        .parse::<ObjectId>()
        .map_err(|_| Error::Identity)?
        .as_bytes();
    let image = record
        .image
        .map(|key| key.parse::<ObjectId>().map_err(|_| Error::Identity))
        .transpose()?;
    Ok((placement, image))
}

pub(crate) fn cleanup_context(
    operation: &Operation,
    journal: &Path,
    binding: [u8; 32],
    manifest: [u8; 32],
) -> Result<(PathBuf, [u8; 32])> {
    let files = view_path(operation, binding)?;
    let marker = journal.join(RETIRED);
    let placement = if marker.try_exists()? {
        read_retirement(&marker, binding)?.0
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
            image: None,
        })
        .unwrap();
        assert_eq!(retirement.len(), 183);
        assert_eq!(
            digest(&retirement),
            "sha256:8066d01f1035ce627d7f92b356b71fd746237ca79cf9e020c4f8b2d0497e9ec6"
        );
        let retirement = serde_json::to_vec(&Retirement {
            version: 2,
            binding: text([0x11; 32]),
            placement: text([0x44; 32]),
            image: Some(text([0x22; 32])),
        })
        .unwrap();
        assert_eq!(
            digest(&retirement),
            "sha256:48259624d97e97efb7d5234915d2c79e864bc972980b72d6f5ad3d6816fb1f87"
        );
    }

    #[test]
    fn revision_images_are_distinct_and_existing_reservations_survive_a_full_cache() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let source = root.path().canonicalize().unwrap();
        let key = |byte| Some(ObjectId::from_bytes([byte; 32]));
        let first = image_path(&source, Some(2), key(1), true).unwrap();
        private::make_directory(&first).unwrap();
        let second = image_path(&source, Some(2), key(2), true).unwrap();
        assert_ne!(first, second);
        assert!(matches!(
            image_path(&source, Some(2), key(3), true),
            Err(Error::ImageLimit)
        ));
        assert_eq!(image_path(&source, Some(2), key(2), true).unwrap(), second);
        assert_eq!(image_path(&source, Some(2), key(1), false).unwrap(), first);
        assert_eq!(
            image_path(&source, None, None, false).unwrap(),
            source.join("image")
        );
    }
}
