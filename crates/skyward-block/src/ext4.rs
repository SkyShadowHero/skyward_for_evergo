//! Relabel an unmounted ext4 image without applying the Recovery system's
//! policy to the new labels. libext2fs preserves the other inode attributes.

use crate::labels::Library;
use anyhow::{ensure, Context, Result};
use std::ffi::{c_char, c_int, c_long, c_uint, c_void, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

type Handle = *mut c_void;
type Open = unsafe extern "C" fn(*const c_char, c_int, c_int, c_uint, Handle, *mut Handle) -> c_long;
type FilesystemOp = unsafe extern "C" fn(Handle) -> c_long;
type Close = unsafe extern "C" fn(*mut Handle) -> c_long;
type OpenAttrs = unsafe extern "C" fn(Handle, u32, *mut Handle) -> c_long;
type SetAttr = unsafe extern "C" fn(Handle, *const c_char, *const c_void, usize) -> c_long;

/// The caller supplies inodes of this device from a read-only mount it has
/// since unmounted. The init inode goes last, after the other attributes
/// have been synced, so an interrupted repair can still be retried.
pub fn label(root: &Path, labels: &[(u32, Vec<u8>)], init: u32) -> Result<()> {
    const READ_WRITE: c_int = 0x01;
    const BLOCKS_64_BIT: c_int = 0x20_000;
    let lib = Library::open(c"libext2fs.so")?;
    let open = unsafe { std::mem::transmute::<*mut c_void, Open>(lib.symbol(c"ext2fs_open")?) };
    let close = unsafe { std::mem::transmute::<*mut c_void, Close>(lib.symbol(c"ext2fs_close_free")?) };
    let read_bitmaps = unsafe { std::mem::transmute::<*mut c_void, FilesystemOp>(lib.symbol(c"ext2fs_read_bitmaps")?) };
    let flush = unsafe { std::mem::transmute::<*mut c_void, FilesystemOp>(lib.symbol(c"ext2fs_flush")?) };
    let open_attrs = unsafe { std::mem::transmute::<*mut c_void, OpenAttrs>(lib.symbol(c"ext2fs_xattrs_open")?) };
    let read_attrs = unsafe { std::mem::transmute::<*mut c_void, FilesystemOp>(lib.symbol(c"ext2fs_xattrs_read")?) };
    let set_attr = unsafe { std::mem::transmute::<*mut c_void, SetAttr>(lib.symbol(c"ext2fs_xattr_set")?) };
    let close_attrs = unsafe { std::mem::transmute::<*mut c_void, Close>(lib.symbol(c"ext2fs_xattrs_close")?) };
    let manager = lib.symbol(c"unix_io_manager")?.cast::<Handle>();
    let path = CString::new(root.as_os_str().as_bytes()).context("the root device path holds a NUL")?;
    let mut fs = std::ptr::null_mut();
    check(unsafe { open(path.as_ptr(), READ_WRITE | BLOCKS_64_BIT, 0, 0, *manager, &mut fs) }).with_context(|| format!("open {} as ext4", root.display()))?;
    let done = (|| -> Result<()> {
        check(unsafe { read_bitmaps(fs) })?;
        for (inode, label) in labels {
            if *inode == init {
                check(unsafe { flush(fs) }).context("sync the root labels before labeling init")?;
            }
            let mut attrs = std::ptr::null_mut();
            check(unsafe { open_attrs(fs, *inode, &mut attrs) }).with_context(|| format!("open attributes for inode {inode}"))?;
            let done = check(unsafe { read_attrs(attrs) }).and_then(|()| check(unsafe {
                set_attr(attrs, c"security.selinux".as_ptr(), label.as_ptr().cast(), label.len())
            }));
            let closed = check(unsafe { close_attrs(&mut attrs) });
            done.and(closed).with_context(|| format!("label inode {inode}"))?;
        }
        Ok(())
    })();
    let closed = check(unsafe { close(&mut fs) }).context("sync and close the labeled root");
    done.and(closed)
}

fn check(code: c_long) -> Result<()> {
    ensure!(code == 0, "libext2fs error {code}");
    Ok(())
}
