//! First-stage work for an Android system carried in the boot partition.

use super::{by_name, log, map_area, mount, partition_info, recovery_in_place, restart, step, stay, take_down, unmount, GIVEN_UP, MISC_NODE, NEW_ROOT, PARTITION_TIMEOUT};
use anyhow::{ensure, Context, Result};
use skyward_block::{area::HEADER, BY_NAME};
use skyward_sys::bcb::{self, Bcb, Work};
use skyward_systems::boot::{Start, ANDROID_AREA_BYTES, ANDROID_BOOT_MARKER, ANDROID_DATA_BYTES, ANDROID_METADATA_BYTES};
use skyward_systems::install::STORE_DEVICE;
use skyward_systems::state::{State, PATH};
use skyward_systems::switch::{self, Places};
use skyward_vab::Slot;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{symlink, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub(super) const BACKING: &str = "/dev/.skyward-backing";
const METADATA: &str = "/metadata";
const FSTAB: &str = "vendor/etc/fstab.mt6833";
const BOOTSTAT: &str = "system/etc/init/bootstat.rc";
const BOOT_HOOK: &str = "\n# Skyward boot confirmation\non property:sys.boot_completed=1\n    write /metadata/bootstat/skyward-boot ready\n";
const BOOT_TIMEOUT: Duration = Duration::from_secs(300);

/// Use the conventional nodes so that fstab, vendor commands and ueventd
/// aliases all see the same private devices. During coldboot, ueventd
/// applies the usual permissions and labels to existing nodes. Binding the
/// nodes to themselves keeps later uevents from unlinking and replacing them.
pub(super) fn prepare_android(start: &Start) -> Result<()> {
    let area = start.android.as_ref().context("switch to Android from the installed system's Recovery to prepare its private storage")?;
    ensure!(area.size == ANDROID_AREA_BYTES && area.partition == start.root.partition, "the recorded Android storage does not match its root");
    let root = Path::new(NEW_ROOT);
    let fstab = fs::read_to_string(root.join(FSTAB)).context("read the second system's fstab.mt6833")?;
    for (part, point) in [("userdata", "/data"), ("metadata", METADATA)] {
        let source = format!("{BY_NAME}/{part}");
        let mut matches = fstab.lines().filter_map(|line| {
            let mut fields = line.split_whitespace();
            let device = fields.next()?;
            (fields.next()? == point).then_some(device)
        });
        ensure!(matches.clone().next().is_some() && matches.all(|dev| dev == source), "the second system's fstab does not use {source} for {point}");
    }

    let mut metadata = None;
    for (part, name, offset, len) in [
        ("userdata", "skyward-android-data", HEADER + ANDROID_METADATA_BYTES, ANDROID_DATA_BYTES),
        ("metadata", "skyward-android-meta", HEADER, ANDROID_METADATA_BYTES),
    ] {
        let (physical, _) = partition_info(part, PARTITION_TIMEOUT)?;
        let node = format!("/dev/block/{physical}");
        ensure!(!Path::new(&node).exists(), "the physical {part} node already exists");
        map_area(area, BACKING, name, &node, offset, len, false)?;
        mount(&node, &node, None, libc::MS_BIND, None)?;
        symlink(&node, format!("{BY_NAME}/{part}"))?;
        if part == "metadata" {
            metadata = Some(node);
        }
    }
    fs::remove_file(BACKING)?;

    let point = format!("{NEW_ROOT}{METADATA}");
    mount(&metadata.expect("the private metadata device"), &point, Some("ext4"), libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOATIME | libc::MS_SYNCHRONOUS, Some("data=journal,commit=1"))?;
    let marker = Path::new(&point).join(ANDROID_BOOT_MARKER);
    match fs::read_to_string(&marker) {
        Ok(status) => ensure!(status == "ready", "the previous Android boot did not reach sys.boot_completed=1"),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("read {}", marker.display())),
    }
    boot_hook(&root.join(BOOTSTAT))?;
    let dir = marker.parent().expect("the marker has a parent");
    fs::create_dir_all(dir)?;
    let mut file = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&marker)?;
    file.write_all(b"starting")?;
    file.sync_all()?;
    File::open(dir)?.sync_all()?;
    Ok(())
}

/// Append to an existing system file so its Android security label stays.
/// A cut-off append can be retried; recovery never changes the stock root.
fn boot_hook(path: &Path) -> Result<()> {
    let mut file = OpenOptions::new().read(true).append(true).custom_flags(libc::O_NOFOLLOW).open(path).with_context(|| format!("open {}", path.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    if text.ends_with(BOOT_HOOK) {
        return Ok(());
    }
    let partial = (1..BOOT_HOOK.len()).rev().find(|&n| text.ends_with(&BOOT_HOOK[..n])).unwrap_or(0);
    file.write_all(&BOOT_HOOK.as_bytes()[partial..])?;
    file.sync_all().with_context(|| format!("sync {}", path.display()))
}

/// Arm before handing off PID 1, without requiring access to Android's
/// property area after SELinux is loaded. init closes this file only when
/// sys.boot_completed=1; the durable value is checked at the next boot.
pub(super) fn watch_android() -> Result<()> {
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("open the Android boot watch");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let marker = super::c(&format!("{METADATA}/{ANDROID_BOOT_MARKER}"));
    if unsafe { libc::inotify_add_watch(fd.as_raw_fd(), marker.as_ptr(), libc::IN_CLOSE_WRITE) } < 0 {
        return Err(std::io::Error::last_os_error()).context("watch Android's boot confirmation");
    }
    match unsafe { libc::fork() } {
        -1 => Err(std::io::Error::last_os_error()).context("start the Android boot watch"),
        0 => {
            let end = Instant::now() + BOOT_TIMEOUT;
            let mut sweep = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            loop {
                let left = end.saturating_duration_since(Instant::now());
                let rc = unsafe { libc::poll(&mut sweep, 1, left.as_millis() as libc::c_int) };
                if rc < 0 && std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                    continue;
                }
                if rc == 0 {
                    let msg = b"<3>skyward system-init: Android did not confirm startup in time; restarting into Recovery\n";
                    unsafe {
                        libc::write(2, msg.as_ptr().cast(), msg.len());
                        libc::sync();
                        libc::syscall(libc::SYS_reboot, libc::LINUX_REBOOT_MAGIC1, libc::LINUX_REBOOT_MAGIC2, libc::LINUX_REBOOT_CMD_RESTART2, c"recovery".as_ptr());
                    }
                }
                unsafe { libc::_exit(0) }
            }
        }
        _ => Ok(()),
    }
}

/// A recovery request from the second Android must never run its wipe or
/// update against the installed system. Only transfer the explicit switch.
fn installed_request(held: Result<Bcb>) -> Bcb {
    match held {
        Ok(held) => {
            let request = held.request();
            match request.work {
                Work::SwitchSystem { to } if to == "installed" => Bcb { command: "boot-recovery".into(), args: vec!["--switch_system=installed".into()] },
                Work::SystemStopped => super::request(request.reason.as_deref().unwrap_or("the second system did not finish starting")),
                Work::Menu if held.args.is_empty() => Bcb { command: "boot-recovery".into(), args: Vec::new() },
                _ => super::request(request.reason.as_deref().unwrap_or("the second system requested Recovery; restored the installed system")),
            }
        }
        Err(_) => super::request("the second system returned to Recovery with an unreadable boot request"),
    }
}

/// Recover with the installed kernel and matching vendor services. The
/// state and boot store can be read without opening its encrypted /data.
pub(super) fn recover(held: Result<Bcb>) -> ! {
    if GIVEN_UP.swap(true, Ordering::SeqCst) {
        stay();
    }
    step("restoring the installed system's boot image before Recovery");
    let result = (|| -> Result<()> {
        let suffix = skyward_block::slot_suffix()?.context("the bootloader did not name a slot")?;
        let slot = Slot::from_suffix(&suffix).with_context(|| format!("unknown boot slot {suffix}"))?;
        by_name(&format!("boot{suffix}"))?;
        by_name("metadata")?;
        fs::create_dir_all(METADATA)?;
        let state_path = Path::new(PATH);
        mount("/dev/block/by-name/metadata", METADATA, Some("ext4"), libc::MS_NOSUID | libc::MS_NODEV, None)?;
        let state = State::load(state_path)?;
        let store = state.store.as_ref().context("no store holds the installed boot image")?;
        let store_node = "/dev/.skyward-store";
        map_area(&store.area, BACKING, STORE_DEVICE, store_node, 0, store.area.size, true)?;
        if let Err(e) = save_previous_console(state_path) {
            log(&format!("previous boot log: {e:#}"));
        }
        let request = installed_request(held);
        bcb::write(Path::new(MISC_NODE), &request)?;
        switch::roll_back_carrier(&Places { state: state_path, by_name: Path::new(BY_NAME), store: Path::new(store_node), program: Path::new("/skyward/system-init") }, slot)?;
        unmount(METADATA, 0)?;
        Ok(())
    })();
    match result {
        Ok(()) => restart(),
        Err(e) => {
            log(&format!("could not restore the installed boot image: {e:#}"));
            let _ = unmount(METADATA, libc::MNT_DETACH);
            // Clear any work from the Android system before entering the
            // recovery ramdisk with the carrier kernel as a last resort.
            let _ = bcb::write(Path::new(MISC_NODE), &super::request(&format!("could not restore the installed boot image: {e:#}")));
            let Err(e) = take_down().and_then(|()| recovery_in_place());
            log(&format!("could not start Recovery from the carrier: {e:#}"));
        }
    }
    stay()
}

fn save_previous_console(state: &Path) -> Result<()> {
    let Ok(file) = File::open("/proc/last_kmsg") else { return Ok(()) };
    let mut log = Vec::new();
    file.take(2 << 20).read_to_end(&mut log)?;
    if log.windows(b"skyward system-init:".len()).any(|bytes| bytes == b"skyward system-init:") {
        fs::write(state.with_file_name("last-system-boot.log"), log)?;
    }
    Ok(())
}
