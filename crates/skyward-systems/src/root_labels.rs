//! Collect the labels of an Android root image that was copied without
//! SELinux attributes, before unmounting it for the on-disk repair.

use anyhow::{ensure, Context, Result};
use skyward_block::labels::Rules;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const FILE_CONTEXTS: [&str; 5] = [
    "system/etc/selinux/plat_file_contexts",
    "system_ext/etc/selinux/system_ext_file_contexts",
    "product/etc/selinux/product_file_contexts",
    "vendor/etc/selinux/vendor_file_contexts",
    "odm/etc/selinux/odm_file_contexts",
];
const INIT_LABEL: &[u8] = b"u:object_r:init_exec:s0\0";

pub(super) type Labels = (Vec<(u32, Vec<u8>)>, u32);

pub(super) fn missing(root: &Path) -> Result<Option<Labels>> {
    let init = root.join("system/bin/init");
    let init_path = CString::new(init.as_os_str().as_bytes())?;
    let mut current = [0u8; 128];
    let size = unsafe { libc::lgetxattr(init_path.as_ptr(), c"security.selinux".as_ptr(), current.as_mut_ptr().cast(), current.len()) };
    if size > 0 && current[..size as usize] == *INIT_LABEL {
        return Ok(None);
    }
    let init = u32::try_from(fs::symlink_metadata(init)?.ino())?;
    let files: Vec<PathBuf> = FILE_CONTEXTS.iter().map(|path| root.join(path)).filter(|path| path.is_file()).map(fs::canonicalize).collect::<std::io::Result<_>>()?;
    ensure!(files.first() == Some(&root.join(FILE_CONTEXTS[0])) && files.iter().all(|path| path.starts_with(root)), "the second system's file contexts must be inside its root");
    let rules = Rules::open(&files)?;
    let mut labels = BTreeMap::new();
    let mut paths = vec![root.to_path_buf()];
    while let Some(path) = paths.pop() {
        let meta = fs::symlink_metadata(&path).with_context(|| format!("look at {}", path.display()))?;
        let rel = path.strip_prefix(root)?;
        if let Some(label) = rules.lookup(&Path::new("/").join(rel), meta.mode())? {
            let inode = u32::try_from(meta.ino())?;
            if let Some(old) = labels.insert(inode, label.clone()) {
                ensure!(old == label, "{} is hard-linked to an entry with a different Android label", path.display());
            }
        }
        if meta.is_dir() {
            for child in fs::read_dir(&path)? {
                paths.push(child?.path());
            }
        }
    }
    let last = labels.remove(&init).context("the Android rules give init no file label")?;
    ensure!(last == INIT_LABEL, "the Android rules give init an unexpected file label");
    let labels = labels.into_iter().chain([(init, last)]).collect();
    Ok(Some((labels, init)))
}
