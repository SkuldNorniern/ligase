//! loading a library on windows: `LoadLibraryW` and `GetProcAddress`.

use std::ffi::{CString, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const i8) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}

/// a loaded library; let go on drop.
pub struct Library(*mut c_void);

// SAFETY: a module handle is a plain address windows lets any thread use.
unsafe impl Send for Library {}

impl Library {
    pub fn open(path: &Path) -> Option<Self> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: `wide` is a nul-ended utf-16 path that lives through the call.
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        (!module.is_null()).then_some(Self(module))
    }

    /// the address of `name`, none when the library has no such export.
    pub fn symbol(&self, name: &str) -> Option<*mut c_void> {
        let name = CString::new(name).ok()?;
        // SAFETY: the module is loaded while `self` lives; `name` is nul-ended.
        let at = unsafe { GetProcAddress(self.0, name.as_ptr()) };
        (!at.is_null()).then_some(at)
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: the handle came from LoadLibraryW and is freed once.
        unsafe {
            FreeLibrary(self.0);
        }
    }
}

/// the file name pdfium has here.
pub const PDFIUM: &str = "pdfium.dll";
