//! loading a library on linux and macos: `dlopen` and `dlsym`.

use std::ffi::{CString, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const RTLD_NOW: i32 = 2;

#[cfg_attr(target_os = "linux", link(name = "dl"))]
unsafe extern "C" {
    fn dlopen(name: *const i8, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const i8) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> i32;
}

/// a loaded library; let go on drop.
pub struct Library(*mut c_void);

// SAFETY: a dlopen handle may be used from any thread.
unsafe impl Send for Library {}

impl Library {
    pub fn open(path: &Path) -> Option<Self> {
        let name = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: `name` is a nul-ended path that lives through the call.
        let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW) };
        (!handle.is_null()).then_some(Self(handle))
    }

    /// the address of `name`, none when the library has no such export.
    pub fn symbol(&self, name: &str) -> Option<*mut c_void> {
        let name = CString::new(name).ok()?;
        // SAFETY: the handle is open while `self` lives; `name` is nul-ended.
        let at = unsafe { dlsym(self.0, name.as_ptr()) };
        (!at.is_null()).then_some(at)
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: the handle came from dlopen and is closed once.
        unsafe {
            dlclose(self.0);
        }
    }
}

/// the file name pdfium has here.
#[cfg(target_os = "macos")]
pub const PDFIUM: &str = "libpdfium.dylib";
#[cfg(not(target_os = "macos"))]
pub const PDFIUM: &str = "libpdfium.so";
