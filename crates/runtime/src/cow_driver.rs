//! One trusted host operator, not an authorization boundary for a hostile host
//! process. The external runtime keeps agents away from all these host paths.
use crate::{Error, OverlayLayout, OverlayView, Result, volume::overlay::driver_mount};
use rustix::{
    fs::{AtFlags, FlockOperation, Mode, OFlags, flock, openat, unlinkat},
    mount::{MountFlags, MountPropagationFlags, UnmountFlags, mount, mount_change, unmount},
};
use std::{
    ffi::{CString, OsString},
    fs::{self, File},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub fn run(args: &[OsString]) -> Result<()> {
    let [action, lower, upper, work, files] = args else {
        return Err(Error::Invalid);
    };
    let mounting = match action.to_str() {
        Some("mount") => true,
        Some("unmount") => false,
        _ => return Err(Error::Invalid),
    };
    let layout = OverlayLayout {
        lower: PathBuf::from(lower),
        upper: PathBuf::from(upper),
        work: PathBuf::from(work),
        files: PathBuf::from(files),
    };
    shape(&layout)?;
    if !rustix::process::geteuid().is_root() {
        let executable = std::env::current_exe()?;
        protected_executable(&executable)?;
        let status = Command::new("/usr/bin/sudo")
            .args(["-n", "--"])
            .arg(executable)
            .args(args)
            .env_clear()
            .stdin(Stdio::null())
            .status()?;
        return if status.success() {
            Ok(())
        } else {
            Err(Error::Unknown)
        };
    }
    // sudo establishes this value from the actual caller. Root itself is a
    // trusted administrator; no claim of confining a malicious root is made.
    let owner: u32 = std::env::var("SUDO_UID")
        .map_err(|_| Error::Invalid)?
        .parse()
        .map_err(|_| Error::Invalid)?;
    if owner == 0 {
        return Err(Error::Invalid);
    }
    let views = layout.files.parent().ok_or(Error::Invalid)?;
    let held_views = owned_directory(views, owner, true)?;
    let lock_name = format!(
        "{}.lock",
        layout
            .files
            .file_name()
            .ok_or(Error::Invalid)?
            .to_str()
            .ok_or(Error::Invalid)?
    );
    let lock = File::from(
        openat(
            &held_views,
            lock_name.as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(std::io::Error::from)?,
    );
    let metadata = lock.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() != 0
        || ![0, owner].contains(&metadata.uid())
        || metadata.mode() & 0o077 != 0
    {
        return Err(Error::Mismatch);
    }
    // The process doing the privileged syscall owns the lock. It survives a
    // lost node/frontend and stays held until even an interrupted syscall ends.
    flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::State)?;
    held_views.sync_all()?;
    let _lower = owned_directory(&layout.lower, owner, true)?;
    let _placement = owned_directory(layout.upper.parent().ok_or(Error::Invalid)?, owner, true)?;
    let present = OverlayView::mount_present(&layout.files)?;
    let _upper = optional_directory(&layout.upper, owner, mounting || present)?;
    let work = optional_directory(&layout.work, owner, mounting || present)?;
    // Holding an open descriptor into the overlay would itself prevent umount.
    drop(optional_directory(
        &layout.files,
        owner,
        mounting || present,
    )?);
    if present {
        driver_mount(&layout, false)?;
    }
    if mounting {
        if !present {
            if fs::read_dir(&layout.files)?.next().is_some()
                || fs::read_dir(&layout.upper)?.next().is_some()
                || fs::read_dir(&layout.work)?.next().is_some()
            {
                return Err(Error::State);
            }
            let options = CString::new(format!(
                "xino=on,metacopy=off,redirect_dir=nofollow,index=off,lowerdir={},upperdir={},workdir={}",
                layout.lower.display(), layout.upper.display(), layout.work.display()
            )).map_err(|_| Error::Invalid)?;
            mount(
                "overlay",
                &layout.files,
                "overlay",
                MountFlags::NODEV | MountFlags::NOSUID,
                options.as_c_str(),
            )
            .map_err(std::io::Error::from)?;
        }
        mount_change(&layout.files, MountPropagationFlags::PRIVATE)
            .map_err(std::io::Error::from)?;
        driver_mount(&layout, true)?;
    } else {
        if present {
            unmount(&layout.files, UnmountFlags::empty()).map_err(std::io::Error::from)?;
        }
        if OverlayView::mount_present(&layout.files)? {
            return Err(Error::State);
        }
        if let Some(work) = work {
            match unlinkat(&work, "work", AtFlags::REMOVEDIR) {
                Ok(()) | Err(rustix::io::Errno::NOENT) => (),
                Err(error) => return Err(std::io::Error::from(error).into()),
            }
            work.sync_all()?;
        }
    }
    Ok(())
}

fn protected_executable(executable: &Path) -> Result<()> {
    if executable.canonicalize()? != executable {
        return Err(Error::Mismatch);
    }
    for path in executable.ancestors() {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || if path == executable {
                !metadata.is_file() || metadata.nlink() != 1
            } else {
                !metadata.is_dir()
            }
        {
            return Err(Error::Mismatch);
        }
    }
    Ok(())
}

fn shape(layout: &OverlayLayout) -> Result<()> {
    for path in [&layout.lower, &layout.upper, &layout.work, &layout.files] {
        let text = path.to_str().ok_or(Error::Invalid)?;
        if !path.is_absolute()
            || !text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
        {
            return Err(Error::Invalid);
        }
    }
    let view = layout
        .files
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or(Error::Invalid)?;
    let views = layout.files.parent().ok_or(Error::Invalid)?;
    let cache = views
        .parent()
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .ok_or(Error::Invalid)?;
    let storage = cache
        .strip_prefix(".treeharbor-cow-")
        .ok_or(Error::Invalid)?;
    let storage = uuid::Uuid::parse_str(storage).map_err(|_| Error::Invalid)?;
    if storage.is_nil()
        || view.len() != 64
        || !view
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || views.file_name().is_none_or(|s| s != "views")
        || layout.lower.file_name().is_none_or(|s| s != "files")
        || layout
            .lower
            .parent()
            .and_then(Path::file_name)
            .is_none_or(|s| s != "image")
        || layout
            .upper
            .file_name()
            .is_none_or(|s| s != "overlay-upper")
        || layout.work.file_name().is_none_or(|s| s != "overlay-work")
        || layout.upper.parent() != layout.work.parent()
    {
        return Err(Error::Invalid);
    }
    let paths = [&layout.lower, &layout.upper, &layout.work, &layout.files];
    for (index, path) in paths.iter().enumerate() {
        if paths
            .iter()
            .enumerate()
            .any(|(other, p)| other != index && path.starts_with(p))
        {
            return Err(Error::Mismatch);
        }
    }
    Ok(())
}

fn owned_directory(path: &Path, owner: u32, private: bool) -> Result<File> {
    if path.canonicalize()? != path {
        return Err(Error::Mismatch);
    }
    let file = File::from(
        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let metadata = file.metadata()?;
    if metadata.uid() != owner || (private && metadata.mode() & 0o077 != 0) {
        return Err(Error::Mismatch);
    }
    Ok(file)
}

fn optional_directory(path: &Path, owner: u32, required: bool) -> Result<Option<File>> {
    match fs::symlink_metadata(path) {
        Ok(_) => owned_directory(path, owner, false).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => Ok(None),
        Err(error) => Err(error.into()),
    }
}
