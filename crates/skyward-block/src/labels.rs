//! Android file-context rules, looked up through the recovery's libselinux.

use anyhow::{ensure, Context, Result};
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[repr(C)]
struct SelinuxOpt {
    kind: c_int,
    value: *const c_char,
}

const SELABEL_CTX_FILE: c_uint = 0;
const SELABEL_OPT_PATH: c_int = 3;

type Open = unsafe extern "C" fn(c_uint, *const SelinuxOpt, c_uint) -> *mut c_void;
type Lookup = unsafe extern "C" fn(*mut c_void, *mut *mut c_char, *const c_char, c_int) -> c_int;
type Close = unsafe extern "C" fn(*mut c_void);
type Free = unsafe extern "C" fn(*mut c_char);

pub struct Rules {
    _lib: Library,
    handle: *mut c_void,
    lookup: Lookup,
    close: Close,
    free: Free,
}

impl Rules {
    /// Read the files in Android's order, later rules overriding earlier ones.
    pub fn open(files: &[PathBuf]) -> Result<Rules> {
        let lib = Library::open(c"libselinux.so")?;
        let open = unsafe { std::mem::transmute::<*mut c_void, Open>(lib.symbol(c"selabel_open")?) };
        let lookup = unsafe { std::mem::transmute::<*mut c_void, Lookup>(lib.symbol(c"selabel_lookup")?) };
        let close = unsafe { std::mem::transmute::<*mut c_void, Close>(lib.symbol(c"selabel_close")?) };
        let free = unsafe { std::mem::transmute::<*mut c_void, Free>(lib.symbol(c"freecon")?) };
        let paths = files.iter().map(|f| CString::new(f.as_os_str().as_bytes())).collect::<Result<Vec<_>, _>>().context("a rules path holds a NUL")?;
        let opts: Vec<SelinuxOpt> = paths.iter().map(|p| SelinuxOpt { kind: SELABEL_OPT_PATH, value: p.as_ptr() }).collect();
        let handle = unsafe { open(SELABEL_CTX_FILE, opts.as_ptr(), opts.len() as c_uint) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error()).with_context(|| format!("read the labeling rules in {files:?}"));
        }
        Ok(Rules { _lib: lib, handle, lookup, close, free })
    }

    /// The label the rules give `path` of type `mode`, with the terminating NUL.
    pub fn lookup(&self, path: &Path, mode: u32) -> Result<Option<Vec<u8>>> {
        let key = CString::new(path.as_os_str().as_bytes()).with_context(|| format!("{} holds a NUL", path.display()))?;
        let mut found: *mut c_char = std::ptr::null_mut();
        if unsafe { (self.lookup)(self.handle, &mut found, key.as_ptr(), mode as c_int) } < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ENOENT) {
                return Ok(None);
            }
            return Err(e).with_context(|| format!("look up the label of {}", path.display()));
        }
        let label = unsafe { CStr::from_ptr(found) }.to_bytes_with_nul().to_vec();
        unsafe { (self.free)(found) };
        Ok(Some(label))
    }
}

impl Drop for Rules {
    fn drop(&mut self) {
        unsafe { (self.close)(self.handle) };
    }
}

pub(crate) struct Library {
    handle: *mut c_void,
    name: &'static CStr,
}

impl Library {
    pub fn open(name: &'static CStr) -> Result<Self> {
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW) };
        ensure!(!handle.is_null(), "load {}: {}", name.to_string_lossy(), unsafe { CStr::from_ptr(libc::dlerror()) }.to_string_lossy());
        Ok(Self { handle, name })
    }

    pub fn symbol(&self, name: &CStr) -> Result<*mut c_void> {
        let symbol = unsafe { libc::dlsym(self.handle, name.as_ptr()) };
        ensure!(!symbol.is_null(), "find {} in {}", name.to_string_lossy(), self.name.to_string_lossy());
        Ok(symbol)
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        unsafe { libc::dlclose(self.handle) };
    }
}
