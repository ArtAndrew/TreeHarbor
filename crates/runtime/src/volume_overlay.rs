use super::{Error, Result, Volume, directory, files_directory, mountinfo, unescape};
use rustix::fs::{AtFlags, StatxFlags, statx};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    os::unix::fs::MetadataExt,
    path::PathBuf,
};
use uuid::Uuid;

/// Operator-provisioned local paths. The caller independently authenticates
/// the immutable lower layer and keeps it pinned for the full workspace life.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayLayout {
    pub lower: PathBuf,
    pub upper: PathBuf,
    pub work: PathBuf,
    pub files: PathBuf,
}

/// Exact observation of a writable view charged to an exclusive ext4 volume.
/// This guard neither mounts files nor accepts a runtime or source identity.
pub struct OverlayView {
    volume: Volume,
    layout: OverlayLayout,
    evidence: Evidence,
    _held: [File; 4],
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct Evidence {
    mount: u64,
    boot: Uuid,
    directories: [DirectoryIdentity; 4],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    unique_mount: u64,
}

impl OverlayView {
    pub fn observe(volume: &Volume, layout: OverlayLayout) -> Result<Self> {
        let volume = volume.try_clone()?;
        let (evidence, held) = inspect(&volume, &layout)?;
        Ok(Self {
            volume,
            layout,
            evidence,
            _held: held,
        })
    }

    pub fn files(&self) -> &std::path::Path {
        &self.layout.files
    }

    pub fn mount_id(&self) -> u64 {
        self.evidence.mount
    }

    pub(crate) fn volume(&self) -> &Volume {
        &self.volume
    }

    pub(crate) fn files_device(&self) -> u64 {
        self.evidence.directories[3].device
    }

    pub(crate) fn intent(&self) -> serde_json::Value {
        serde_json::json!({"layout": self.layout, "observed": self.evidence})
    }

    pub fn verify(&self) -> Result<()> {
        let (current, _) = inspect(&self.volume, &self.layout)?;
        if current != self.evidence {
            return Err(Error::Mismatch);
        }
        Ok(())
    }
}

fn inspect(volume: &Volume, layout: &OverlayLayout) -> Result<(Evidence, [File; 4])> {
    volume.verify()?;
    let mount = &volume.identity().mount;
    if layout.upper == layout.work
        || layout.upper.parent() != layout.work.parent()
        || ![&layout.upper, &layout.work]
            .into_iter()
            .all(|path| path != volume.files() && path.starts_with(volume.files()))
        || [&layout.lower, &layout.files]
            .into_iter()
            .any(|path| path.starts_with(mount) || mount.starts_with(path))
        || layout.files.starts_with(&layout.lower)
        || layout.lower.starts_with(&layout.files)
    {
        return Err(Error::Mismatch);
    }
    let paths = [&layout.lower, &layout.upper, &layout.work, &layout.files];
    for path in paths {
        // Use canonical, single-layer paths with unambiguous mount options.
        let name = path.to_str().ok_or(Error::Unsupported)?;
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
            || path.canonicalize()? != *path
        {
            return Err(Error::Mismatch);
        }
    }
    let held = [
        directory(&layout.lower)?,
        directory(&layout.upper)?,
        directory(&layout.work)?,
        files_directory(&layout.files)?,
    ];
    let mut directories = [DirectoryIdentity::default(); 4];
    for (file, identity) in held.iter().zip(&mut directories) {
        let metadata = file.metadata()?;
        *identity = DirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            unique_mount: unique_mount(file)?,
        };
    }
    let device = volume.identity().device;
    if directories[1].device != device
        || directories[2].device != device
        || directories[3].device == device
        || directories[1].unique_mount != directories[2].unique_mount
    {
        return Err(Error::Mismatch);
    }
    let stat = rustix::fs::fstatfs(&held[3]).map_err(std::io::Error::from)?;
    if stat.f_type != 0x794c7630 {
        return Err(Error::Unsupported);
    }
    let capacity = stat
        .f_blocks
        .checked_mul(stat.f_bsize.try_into().map_err(|_| Error::Invalid)?)
        .ok_or(Error::Invalid)?;
    if capacity != volume.identity().capacity_bytes {
        return Err(Error::Mismatch);
    }
    let mount = record(&mountinfo()?, layout, directories[3].device)?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let boot = Uuid::parse_str(boot.trim()).map_err(|_| Error::Unsupported)?;
    Ok((
        Evidence {
            mount,
            boot,
            directories,
        },
        held,
    ))
}

fn unique_mount(file: &File) -> Result<u64> {
    // STATX_MNT_ID_UNIQUE is not named by the pinned rustix StatxFlags.
    // The legacy mountinfo ID can be recycled immediately after unmount.
    const UNIQUE: StatxFlags = StatxFlags::from_bits_retain(0x4000);
    let stat = statx(file, c"", AtFlags::EMPTY_PATH, UNIQUE).map_err(std::io::Error::from)?;
    if stat.stx_mask & UNIQUE.bits() == 0 || stat.stx_mnt_id == 0 {
        return Err(Error::Unsupported);
    }
    Ok(stat.stx_mnt_id)
}

fn record(text: &str, layout: &OverlayLayout, device: rustix::fs::Dev) -> Result<u64> {
    let mut found = None;
    for line in text.lines() {
        let fields: Vec<_> = line.split(' ').collect();
        if fields.len() < 10 {
            return Err(Error::Invalid);
        }
        let point = unescape(fields[4])?;
        if point != layout.files {
            if point.starts_with(&layout.files) {
                return Err(Error::Mismatch);
            }
            continue;
        }
        if found.is_some() {
            return Err(Error::Mismatch);
        }
        let split = fields
            .iter()
            .position(|field| *field == "-")
            .ok_or(Error::Invalid)?;
        if split != 6
            || fields.len() != split + 4
            || fields[3] != "/"
            || fields[7] != "overlay"
            || fields[8] != "overlay"
        {
            return Err(Error::Mismatch);
        }
        let options: Vec<_> = fields[5].split(',').collect();
        if !["rw", "nodev", "nosuid"]
            .iter()
            .all(|option| options.contains(option))
        {
            return Err(Error::Mismatch);
        }
        let options: Vec<_> = fields[9].split(',').collect();
        for (key, path) in [
            ("lowerdir=", &layout.lower),
            ("upperdir=", &layout.upper),
            ("workdir=", &layout.work),
        ] {
            let mut values = options.iter().filter_map(|option| option.strip_prefix(key));
            if values.next() != path.to_str() || values.next().is_some() {
                return Err(Error::Mismatch);
            }
        }
        if !["rw", "xino=on", "redirect_dir=nofollow", "nouserxattr"]
            .iter()
            .all(|option| options.contains(option))
            || options.iter().any(|option| {
                ![
                    "rw",
                    "xino=on",
                    "redirect_dir=nofollow",
                    "nouserxattr",
                    "uuid=on",
                    "index=off",
                    "metacopy=off",
                ]
                .contains(option)
                    && !["lowerdir=", "upperdir=", "workdir="]
                        .iter()
                        .any(|key| option.starts_with(key))
            })
        {
            return Err(Error::Mismatch);
        }
        let (major, minor) = fields[2].split_once(':').ok_or(Error::Invalid)?;
        if major.parse::<u32>().map_err(|_| Error::Invalid)? != rustix::fs::major(device)
            || minor.parse::<u32>().map_err(|_| Error::Invalid)? != rustix::fs::minor(device)
        {
            return Err(Error::Mismatch);
        }
        found = Some(fields[0].parse().map_err(|_| Error::Invalid)?);
    }
    found.ok_or(Error::Mismatch)
}
