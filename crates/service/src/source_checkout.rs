//! Assemble one live workspace's registered source selection on the caller's host.
use crate::{
    Error, Result,
    client::{self, Credential, Endpoint},
    download::{self, SourceReply},
    files,
    pack::hex,
    source_read::{decode_hex, mode_name},
};
use rustix::fs::{OFlags, RenameFlags, renameat_with};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink},
    },
    path::Path,
    str::FromStr,
    time::Duration,
};
use tokio::time::{Instant, timeout_at};
use treeharbor_core::{
    Directory, Entry, EntryMode, EntryRef, Name, Object, ObjectId, ProfileId, RepoPath,
    RevisionRef, SourceProfile,
};
use treeharbor_materializer::Selection;
use treeharbor_metadata::{commands::ProfileWire, workspace_reads::SourceFence};

const PAGE_ENTRIES: usize = 256;
const MAX_PATH_BYTES: usize = 64 * 1024 * 1024;

pub(crate) struct Source<'a> {
    pub endpoint: Endpoint<'a>,
    pub credential: &'a Credential,
    pub fence: SourceFence,
    pub request_timeout: Duration,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    base: RevisionRef,
    snapshot: ObjectId,
    profile: ProfileId,
}

impl Identity {
    fn read(result: &Value) -> Result<Self> {
        let base = field(result, "base")?;
        if !matches!(base, RevisionRef::Native(_)) {
            return Err(Error::CheckoutState);
        }
        Ok(Self {
            base,
            snapshot: field(result, "snapshot")?,
            profile: field(result, "profile_digest")?,
        })
    }
}

fn field<T: FromStr>(value: &Value, key: &str) -> Result<T> {
    value[key]
        .as_str()
        .ok_or(Error::CheckoutState)?
        .parse()
        .map_err(|_| Error::CheckoutState)
}

#[derive(Deserialize)]
struct WireEntry {
    name_hex: String,
    mode: String,
    object: String,
}

impl WireEntry {
    fn decode(self) -> Result<Entry> {
        let name = decode_hex(&self.name_hex, 255).map_err(|_| Error::CheckoutState)?;
        let mode = match self.mode.as_str() {
            "040000" => EntryMode::Directory,
            "100644" => EntryMode::File,
            "100755" => EntryMode::Executable,
            "120000" => EntryMode::Symlink,
            _ => return Err(Error::CheckoutState),
        };
        Ok(Entry {
            name: Name::new(name).map_err(|_| Error::CheckoutState)?,
            value: EntryRef {
                mode,
                target: self.object.parse().map_err(|_| Error::CheckoutState)?,
            },
        })
    }
}

#[derive(Deserialize)]
struct WirePage {
    entries: Vec<WireEntry>,
    next_after_hex: Option<String>,
}

struct Page {
    identity: Identity,
    object: ObjectId,
    wire: WirePage,
}

impl Source<'_> {
    async fn read(&self, operation: Value) -> Result<SourceReply> {
        let reply = download::read_from(
            self.endpoint,
            self.credential,
            self.fence,
            operation,
            self.request_timeout,
        )
        .await?;
        if !reply.status.is_success() {
            return Err(Error::CheckoutRejected {
                status: reply.status.as_u16(),
            });
        }
        Ok(reply)
    }

    async fn list(&self, path_hex: &str, after: Option<&str>, limit: usize) -> Result<Page> {
        let mut reply = self
            .read(json!({"kind":"list","path_hex":path_hex,"after_hex":after,"limit":limit}))
            .await?;
        let result = reply.metadata["result"].take();
        if result["path_hex"] != path_hex {
            return Err(Error::CheckoutState);
        }
        let identity = Identity::read(&result)?;
        let object = field(&result, "object")?;
        let wire: WirePage = serde_json::from_value(result).map_err(|_| Error::CheckoutState)?;
        if wire.entries.len() > limit
            || (after.is_some() && wire.entries.is_empty())
            || wire.next_after_hex.as_ref().is_some_and(|cursor| {
                wire.entries.len() != limit
                    || wire
                        .entries
                        .last()
                        .is_none_or(|last| &last.name_hex != cursor)
            })
        {
            return Err(Error::CheckoutState);
        }
        Ok(Page {
            identity,
            object,
            wire,
        })
    }

    async fn directory(
        &self,
        path: &[u8],
        expected: Option<(Identity, ObjectId)>,
        remaining_entries: &mut usize,
    ) -> Result<(Identity, ObjectId, Directory)> {
        let path_hex = hex(path);
        let mut page = self.list(&path_hex, None, PAGE_ENTRIES).await?;
        let (identity, object) = (page.identity, page.object);
        if let Some(expected) = expected {
            if expected != (identity, object) {
                return Err(Error::CheckoutState);
            }
        } else if (Object::Snapshot { root: object })
            .id()
            .map_err(|_| Error::CheckoutState)?
            != identity.snapshot
        {
            return Err(Error::CheckoutState);
        }
        let mut entries: Vec<Entry> = Vec::new();
        loop {
            if (page.identity, page.object) != (identity, object) {
                return Err(Error::CheckoutState);
            }
            *remaining_entries = remaining_entries
                .checked_sub(page.wire.entries.len())
                .ok_or(Error::Size)?;
            for wire in page.wire.entries {
                let entry = wire.decode()?;
                if entries.last().is_some_and(|last| last.name >= entry.name) {
                    return Err(Error::CheckoutState);
                }
                entries.push(entry);
            }
            let Some(next) = page.wire.next_after_hex else {
                break;
            };
            page = self.list(&path_hex, Some(&next), PAGE_ENTRIES).await?;
        }
        let directory = Directory::new(entries).map_err(|_| Error::CheckoutState)?;
        if Object::Directory(directory.clone())
            .id()
            .map_err(|_| Error::CheckoutState)?
            != object
        {
            return Err(Error::CheckoutState);
        }
        Ok((identity, object, directory))
    }

    async fn profile(&self, expected: ProfileId) -> Result<SourceProfile> {
        let request = self.credential.request(json!({
            "method":"source_profile_get","profile_digest":expected.to_string()
        }));
        let mut reply = client::send_to(
            self.endpoint,
            &self.credential.token,
            request.to_string().into_bytes(),
            self.request_timeout,
        )
        .await?;
        if !reply.status.is_success() {
            return Err(Error::CheckoutRejected {
                status: reply.status.as_u16(),
            });
        }
        if reply.body["version"] != 1 {
            return Err(Error::CheckoutState);
        }
        let result = reply.body.get_mut("result").ok_or(Error::CheckoutState)?;
        if field::<ProfileId>(result, "profile_digest")? != expected {
            return Err(Error::CheckoutState);
        }
        let wire: ProfileWire =
            serde_json::from_value(result["profile"].take()).map_err(|_| Error::CheckoutState)?;
        let profile = wire.decode().map_err(|_| Error::CheckoutState)?;
        if profile.id() != expected {
            return Err(Error::CheckoutState);
        }
        Ok(profile)
    }

    async fn file(&self, path: &RepoPath, identity: Identity, entry: EntryRef) -> Result<Vec<u8>> {
        let reply = self
            .read(json!({"kind":"file","path_hex":hex(path.as_bytes())}))
            .await?;
        let result = &reply.metadata["result"];
        if Identity::read(result)? != identity
            || field::<ObjectId>(result, "object")? != entry.target
            || result["mode"] != mode_name(entry.mode)
        {
            return Err(Error::CheckoutState);
        }
        reply.content.ok_or(Error::CheckoutState)
    }

    async fn prepare(&self, stage: &Path, max_bytes: u64) -> Result<Value> {
        let mut remaining_entries = treeharbor_core::MAX_ENTRIES;
        let (identity, root, directory) = self.directory(b"", None, &mut remaining_entries).await?;
        let profile = self.profile(identity.profile).await?;
        let selection = Selection::from_profile(&profile);
        let mut missing: BTreeSet<_> = profile
            .paths()
            .unwrap_or_default()
            .iter()
            .cloned()
            .collect();
        let mut pending = Vec::new();
        let mut path_bytes = 0;
        queue(&mut pending, &directory, b"", &selection, &mut path_bytes)?;
        let mut directories = vec![stage.to_owned()];
        let mut links = BTreeMap::new();
        let mut bytes = 0;
        let mut entries = 0;
        while let Some((path, entry)) = pending.pop() {
            missing.remove(&path);
            entries += 1;
            let destination = stage.join(OsStr::from_bytes(path.as_bytes()));
            if entry.mode == EntryMode::Directory {
                let (_, _, directory) = self
                    .directory(
                        path.as_bytes(),
                        Some((identity, entry.target)),
                        &mut remaining_entries,
                    )
                    .await?;
                fs::DirBuilder::new().mode(0o700).create(&destination)?;
                directories.push(destination);
                queue(
                    &mut pending,
                    &directory,
                    path.as_bytes(),
                    &selection,
                    &mut path_bytes,
                )?;
                continue;
            }
            let content = self.file(&path, identity, entry).await?;
            bytes += content.len() as u64;
            if bytes > max_bytes {
                return Err(Error::Size);
            }
            if entry.mode == EntryMode::Symlink {
                symlink(OsStr::from_bytes(&content), &destination)?;
                links.insert(path.as_bytes().to_vec(), content);
            } else {
                let mode = if entry.mode == EntryMode::Executable {
                    0o700
                } else {
                    0o600
                };
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(mode)
                    .open(&destination)?;
                file.write_all(&content)?;
                file.sync_all()?;
            }
        }
        if !missing.is_empty() {
            return Err(Error::CheckoutState);
        }
        treeharbor_materializer::validate_workspace_links(stage, &links)
            .map_err(Error::CheckoutFilesystem)?;
        for directory in directories.iter().rev() {
            File::open(directory)?.sync_all()?;
        }
        // Re-check the live fence after staging, before the only publication.
        let last = self.list("", None, 1).await?;
        if (last.identity, last.object) != (identity, root) {
            return Err(Error::CheckoutState);
        }
        Ok(json!({"version":1,"result":{
            "workspace":self.fence.workspace,"generation":self.fence.generation,
            "authority":self.fence.authority.as_str(),"epoch":self.fence.epoch,
            "base":identity.base.to_string(),"snapshot":identity.snapshot.to_string(),
            "profile_digest":identity.profile.to_string(),"entries":entries,"bytes":bytes
        }}))
    }

    pub async fn checkout(
        &self,
        output: &Path,
        max_bytes: u64,
        timeout: Duration,
    ) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        let parent = files::private_directory(output.parent().ok_or(Error::CheckoutOutput)?)?;
        let name = output.file_name().ok_or(Error::CheckoutOutput)?;
        match parent.join(name).symlink_metadata() {
            Ok(_) => return Err(Error::CheckoutOutput),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let parent_fd = OpenOptions::new()
            .read(true)
            .custom_flags((OFlags::DIRECTORY | OFlags::NOFOLLOW).bits() as i32)
            .open(&parent)?;
        let stage = tempfile::Builder::new()
            .prefix(".treeharbor-checkout-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(&parent)?;
        let receipt = timeout_at(deadline, self.prepare(stage.path(), max_bytes))
            .await
            .map_err(|_| Error::CheckoutDeadline)??;
        if Instant::now() >= deadline {
            return Err(Error::CheckoutDeadline);
        }
        renameat_with(
            &parent_fd,
            stage.path().file_name().ok_or(Error::CheckoutOutput)?,
            &parent_fd,
            name,
            RenameFlags::NOREPLACE,
        )
        .map_err(|error| {
            let error = io::Error::from(error);
            if error.kind() == io::ErrorKind::AlreadyExists {
                Error::CheckoutOutput
            } else {
                error.into()
            }
        })?;
        let _ = stage.keep();
        parent_fd
            .sync_all()
            .map_err(|_| Error::CheckoutDurability)?;
        Ok(receipt)
    }
}

fn queue(
    pending: &mut Vec<(RepoPath, EntryRef)>,
    directory: &Directory,
    parent: &[u8],
    selection: &Selection,
    path_bytes: &mut usize,
) -> Result<()> {
    for entry in directory.entries().iter().rev() {
        let path = if parent.is_empty() {
            entry.name.as_bytes().to_vec()
        } else {
            [parent, b"/", entry.name.as_bytes()].concat()
        };
        let path = RepoPath::new(path).map_err(|_| Error::CheckoutState)?;
        if selection.relevant(&path) {
            *path_bytes += path.as_bytes().len();
            if *path_bytes > MAX_PATH_BYTES {
                return Err(Error::Size);
            }
            pending.push((path, entry.value));
        }
    }
    Ok(())
}
