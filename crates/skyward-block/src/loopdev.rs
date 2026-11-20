//! Making a block device out of part of a file, which is how the bundles a
//! phone keeps its shared libraries in are opened.

#[cfg(target_os = "android")]
pub use android::*;

#[cfg(target_os = "android")]
mod android {
    use anyhow::{Context, Result};
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::path::{Path, PathBuf};

    const CONTROL: &str = "/dev/loop-control";

    const GET_FREE: u32 = 0x4C82;
    const CONFIGURE: u32 = 0x4C0A;
    /// The older way, for kernels before the one that took a whole
    /// configuration at once: point the device at the file, then give it the
    /// offset and the length.
    const SET_FD: u32 = 0x4C00;
    const SET_STATUS64: u32 = 0x4C04;
    const CLR_FD: u32 = 0x4C01;

    /// The device only ever reads. It stays pointed at the file until the
    /// recovery ends, which is as long as anything mounted from it lasts.
    const READ_ONLY: u32 = 1;

    /// Every field is a plain number or a run of bytes, so the request is
    /// made by zeroing it and filling in what matters.
    #[repr(C)]
    struct Info {
        device: u64,
        inode: u64,
        rdevice: u64,
        offset: u64,
        size_limit: u64,
        number: u32,
        encrypt_type: u32,
        encrypt_key_size: u32,
        flags: u32,
        file_name: [u8; 64],
        crypt_name: [u8; 64],
        encrypt_key: [u8; 32],
        init: [u64; 2],
    }

    #[repr(C)]
    struct Config {
        fd: u32,
        block_size: u32,
        info: Info,
        reserved: [u64; 8],
    }

    /// Open `file` from `offset` for `length` bytes as a block device, and
    /// give its path. The device goes away once whatever is mounted from it
    /// is unmounted.
    pub fn attach(file: &Path, offset: u64, length: u64) -> Result<PathBuf> {
        let backing = File::open(file).with_context(|| format!("open {}", file.display()))?;
        let control = File::open(CONTROL).with_context(|| format!("open {CONTROL}"))?;
        let number = unsafe { libc::ioctl(control.as_raw_fd(), GET_FREE as libc::Ioctl) };
        if number < 0 {
            return Err(std::io::Error::last_os_error()).context("ask for a free loop device");
        }
        let path = free_device(number as u32)?;
        let device = File::options()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let mut config: Config = unsafe { std::mem::zeroed() };
        config.fd = backing.as_raw_fd() as u32;
        config.info.offset = offset;
        config.info.size_limit = length;
        config.info.flags = READ_ONLY;
        if unsafe { libc::ioctl(device.as_raw_fd(), CONFIGURE as libc::Ioctl, &mut config) } < 0 {
            let e = std::io::Error::last_os_error();
            // Linux before 5.8 has no LOOP_CONFIGURE. Android 11 and 12
            // phones still run 4.14 or 4.19, where the file is pointed at
            // the device first and its place in the file given after.
            if !matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOTTY)) {
                return Err(e).with_context(|| format!("point {} at {}", path.display(), file.display()));
            }
            configure_legacy(&device, &path, file, &backing, offset, length)?;
        }
        Ok(path)
    }


    /// Point `device` at `backing` and give it `offset` and `length` through
    /// the loop driver's older two steps.
    fn configure_legacy(device: &File, path: &Path, file: &Path, backing: &File, offset: u64, length: u64) -> Result<()> {
        use std::os::fd::AsRawFd;
        if unsafe { libc::ioctl(device.as_raw_fd(), SET_FD as libc::Ioctl, backing.as_raw_fd() as libc::c_ulong) } < 0 {
            let e = std::io::Error::last_os_error();
            // Some MediaTek kernels refuse the ioctl yet hand the same
            // device to the reference tool, which uses the driver's own
            // sequence; take that way when it is there.
            if let Some(done) = by_losetup(path, file, offset, length) {
                return done;
            }
            return Err(e).with_context(|| format!("point {} at {}", path.display(), file.display()));
        }
        let mut info: Info = unsafe { std::mem::zeroed() };
        info.offset = offset;
        info.size_limit = length;
        info.flags = READ_ONLY;
        if unsafe { libc::ioctl(device.as_raw_fd(), SET_STATUS64 as libc::Ioctl, &mut info) } < 0 {
            let e = std::io::Error::last_os_error();
            let _ = unsafe { libc::ioctl(device.as_raw_fd(), CLR_FD as libc::Ioctl) };
            return Err(e).with_context(|| format!("give {} its offset and length", path.display()));
        }
        Ok(())
    }


    /// Point `path` at `file` through the recovery's own `losetup`, where the
    /// kernel refuses the ioctl. `None` where the tool is not there; the
    /// caller then gives the ioctl's error.
    fn by_losetup(path: &Path, file: &Path, offset: u64, length: u64) -> Option<Result<()>> {
        let tool = ["/system/bin/losetup", "/sbin/losetup"].into_iter().find(|p| Path::new(p).exists())?;
        let _ = std::process::Command::new(tool).args(["-d"]).arg(path).status();
        let done = std::process::Command::new(tool)
            .args(["-r", "-o", &offset.to_string(), "--sizelimit", &length.to_string()])
            .arg(path)
            .arg(file)
            .status();
        Some(match done {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(anyhow::anyhow!("{tool} exited with {status}")),
            Err(e) => Err(anyhow::Error::new(e).context(format!("run {tool}"))),
        })
    }

    /// Where the kernel put the loop device it just handed out: Android
    /// keeps its block devices in their own directory.
    fn free_device(number: u32) -> Result<PathBuf> {
        for dir in ["/dev/block", "/dev"] {
            let path = Path::new(dir).join(format!("loop{number}"));
            if path.exists() {
                return Ok(path);
            }
        }
        anyhow::bail!("the kernel handed out loop device {number}, which is in neither /dev/block nor /dev")
    }
}
