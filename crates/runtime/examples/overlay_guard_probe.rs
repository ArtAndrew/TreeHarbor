#[cfg(target_os = "linux")]
fn main() {
    use std::{fs, path::PathBuf, process::Command, time::Instant};
    use treeharbor_runtime::{Error, OverlayLayout, OverlayView, Volume};

    let root = PathBuf::from(std::env::args_os().nth(1).expect("private fixture root"));
    let slot = root.join("volume/slot");
    let volume = Volume::open(&root.join("volume"), &slot, 32 << 20).expect("registered ext4");
    let layout = |lower: &str| OverlayLayout {
        lower: root.join(lower),
        upper: slot.join("upper"),
        work: slot.join("work"),
        files: root.join("merged"),
    };
    assert!(matches!(
        OverlayView::observe(&volume, layout("other")),
        Err(Error::Mismatch)
    ));
    let view = OverlayView::observe(&volume, layout("lower")).expect("exact overlay layout");
    assert_ne!(view.mount_id(), volume.identity().mount_id);
    assert_eq!(fs::read(view.files().join("demo")).unwrap(), b"original\n");
    fs::write(view.files().join("demo"), b"edited\n").unwrap();
    assert_eq!(fs::read(slot.join("upper/demo")).unwrap(), b"edited\n");
    assert_eq!(fs::read(root.join("lower/demo")).unwrap(), b"original\n");

    let injected = view.files().join("injected");
    fs::create_dir(&injected).unwrap();
    assert!(
        Command::new("/usr/bin/sudo")
            .args(["-n", "/usr/bin/mount", "--bind"])
            .arg(root.join("other"))
            .arg(&injected)
            .status()
            .unwrap()
            .success()
    );
    assert!(matches!(view.verify(), Err(Error::Mismatch)));
    assert!(
        Command::new("/usr/bin/sudo")
            .args(["-n", "/usr/bin/umount", "--"])
            .arg(&injected)
            .status()
            .unwrap()
            .success()
    );

    let mut observations = Vec::new();
    for _ in 0..100 {
        let start = Instant::now();
        view.verify().unwrap();
        observations.push(start.elapsed().as_micros());
    }
    observations.sort_unstable();
    println!(
        "Exact ext4 plus overlay revalidation, p50 {} us, p95 {} us (100 observations)",
        observations[49], observations[94],
    );
}

#[cfg(not(target_os = "linux"))]
fn main() {}
