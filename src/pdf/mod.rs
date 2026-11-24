//! pdf pages drawn by pdfium, a c library (bsd-3 / apache-2.0) loaded the
//! first time a pdf opens, never before. a small safe face over the dozen
//! calls used: open a document from memory, page sizes, a page into rgba.
//!
//! pdfium is one global state inside, so the app keeps one [`Engine`] behind
//! a mutex and every call goes through it.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aurea::render::Image;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix::{Library, PDFIUM};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::{Library, PDFIUM};

/// most pages a document is read for.
const MAX_PAGES: i32 = 20_000;
/// biggest page bitmap, in pixels.
const MAX_PIXELS: i64 = 40_000_000;
/// biggest side of a page bitmap.
const MAX_SIDE: f32 = 12_000.0;

const FPDF_ANNOT: i32 = 0x01;
const FPDF_REVERSE_BYTE_ORDER: i32 = 0x10;
const FPDF_BITMAP_BGRA: i32 = 4;

type Doc = *mut c_void;
type Page = *mut c_void;
type Bitmap = *mut c_void;

/// the calls, typed. `extern "system"` is pdfium's `FPDF_CALLCONV`.
struct Calls {
    load_mem: unsafe extern "system" fn(*const c_void, usize, *const i8) -> Doc,
    close_doc: unsafe extern "system" fn(Doc),
    last_error: unsafe extern "system" fn() -> u32,
    page_count: unsafe extern "system" fn(Doc) -> i32,
    load_page: unsafe extern "system" fn(Doc, i32) -> Page,
    close_page: unsafe extern "system" fn(Page),
    page_w: unsafe extern "system" fn(Page) -> f32,
    page_h: unsafe extern "system" fn(Page) -> f32,
    bitmap_new: unsafe extern "system" fn(i32, i32, i32, *mut c_void, i32) -> Bitmap,
    bitmap_fill: unsafe extern "system" fn(Bitmap, i32, i32, i32, i32, u32) -> i32,
    bitmap_buffer: unsafe extern "system" fn(Bitmap) -> *mut c_void,
    bitmap_stride: unsafe extern "system" fn(Bitmap) -> i32,
    bitmap_drop: unsafe extern "system" fn(Bitmap),
    render: unsafe extern "system" fn(Bitmap, Page, i32, i32, i32, i32, i32, i32),
    destroy: unsafe extern "system" fn(),
}

/// pdfium, loaded and set up. one per app.
pub struct Engine {
    calls: Calls,
    // kept last: the calls point into it
    _lib: Library,
}

/// the app's engine slot: empty until the first pdf, shared with workers.
pub type Slot = Arc<Mutex<Option<Engine>>>;

/// where pdfium is looked for: `LIGASE_PDFIUM`, beside the program, the
/// config folder, then the system's own search.
fn places() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("LIGASE_PDFIUM") {
        out.push(PathBuf::from(p));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        out.push(dir.join(PDFIUM));
        // a macos bundle keeps libraries in Frameworks, beside MacOS
        out.push(dir.join("../Frameworks").join(PDFIUM));
    }
    if let Some(dir) = aurea::config_dir("ligase") {
        out.push(dir.join(PDFIUM));
    }
    out.push(PathBuf::from(PDFIUM));
    out
}

/// the export `name` of `lib` as the function type `T`.
fn call<T: Copy>(lib: &Library, name: &str) -> Result<T, String> {
    let at = lib
        .symbol(name)
        .ok_or_else(|| format!("pdfium has no {name}"))?;
    if std::mem::size_of::<T>() != std::mem::size_of::<*mut c_void>() {
        return Err(format!("{name}: not a function pointer"));
    }
    // SAFETY: `T` is the `extern "system"` signature pdfium's headers give
    // this export (see `Calls`), the same size as the address, and `at` is
    // not null.
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&at) })
}

impl Engine {
    /// finds and starts pdfium. worker only: it reads the disk.
    pub fn load() -> Result<Self, String> {
        let lib = places()
            .into_iter()
            .find_map(|p| Library::open(&p))
            .ok_or_else(|| {
                format!("{PDFIUM} was not found; put it beside ligase or set LIGASE_PDFIUM")
            })?;
        let init: unsafe extern "system" fn() = call(&lib, "FPDF_InitLibrary")?;
        let calls = Calls {
            load_mem: call(&lib, "FPDF_LoadMemDocument64")?,
            close_doc: call(&lib, "FPDF_CloseDocument")?,
            last_error: call(&lib, "FPDF_GetLastError")?,
            page_count: call(&lib, "FPDF_GetPageCount")?,
            load_page: call(&lib, "FPDF_LoadPage")?,
            close_page: call(&lib, "FPDF_ClosePage")?,
            page_w: call(&lib, "FPDF_GetPageWidthF")?,
            page_h: call(&lib, "FPDF_GetPageHeightF")?,
            bitmap_new: call(&lib, "FPDFBitmap_CreateEx")?,
            bitmap_fill: call(&lib, "FPDFBitmap_FillRect")?,
            bitmap_buffer: call(&lib, "FPDFBitmap_GetBuffer")?,
            bitmap_stride: call(&lib, "FPDFBitmap_GetStride")?,
            bitmap_drop: call(&lib, "FPDFBitmap_Destroy")?,
            render: call(&lib, "FPDF_RenderPageBitmap")?,
            destroy: call(&lib, "FPDF_DestroyLibrary")?,
        };
        // SAFETY: called once, before any other pdfium call.
        unsafe { init() };
        Ok(Self { calls, _lib: lib })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: every book holds the slot, so none is open past this.
        unsafe { (self.calls.destroy)() };
    }
}

/// an open pdf: its bytes, which pdfium reads from, and its page sizes in
/// points. closes itself through the engine.
pub struct Book {
    slot: Slot,
    doc: usize,
    _bytes: Arc<[u8]>,
    pub pages: Vec<(f32, f32)>,
}

/// pdfium's error code, said.
fn said(code: u32) -> &'static str {
    match code {
        2 => "the file is not there",
        3 => "not a pdf, or a broken one",
        4 => "it has a password",
        5 => "its security is not supported",
        6 => "a page is broken",
        _ => "pdfium could not read it",
    }
}

/// opens `bytes` as a pdf. worker only.
pub fn open(slot: &Slot, bytes: Arc<[u8]>) -> Result<Book, String> {
    let mut guard = slot.lock().map_err(|_| "the pdf engine broke".to_owned())?;
    if guard.is_none() {
        *guard = Some(Engine::load()?);
    }
    let Some(engine) = guard.as_ref() else {
        return Err("no pdf engine".to_owned());
    };
    let c = &engine.calls;
    // SAFETY: `bytes` outlives the document: the book keeps it until close.
    let doc = unsafe { (c.load_mem)(bytes.as_ptr().cast(), bytes.len(), std::ptr::null()) };
    if doc.is_null() {
        // SAFETY: plain query of the last error.
        return Err(said(unsafe { (c.last_error)() }).to_owned());
    }
    // SAFETY: `doc` is open; each page is closed right after it is read.
    let count = unsafe { (c.page_count)(doc) }.clamp(0, MAX_PAGES);
    let mut pages = Vec::with_capacity(count as usize);
    for i in 0..count {
        // SAFETY: as above, `i` is below the page count.
        let size = unsafe {
            let page = (c.load_page)(doc, i);
            if page.is_null() {
                (612.0, 792.0)
            } else {
                let s = ((c.page_w)(page), (c.page_h)(page));
                (c.close_page)(page);
                s
            }
        };
        let ok = |v: f32| {
            if v.is_finite() && v > 1.0 {
                v.min(20_000.0)
            } else {
                612.0
            }
        };
        pages.push((ok(size.0), ok(size.1)));
    }
    drop(guard);
    Ok(Book {
        slot: Arc::clone(slot),
        doc: doc as usize,
        _bytes: bytes,
        pages,
    })
}

impl Book {
    /// page `index` drawn at `scale` pixels per point, on white. worker only.
    pub fn render(&self, index: usize, scale: f32) -> Result<Image, String> {
        let &(pw, ph) = self.pages.get(index).ok_or("no such page")?;
        let scale = scale.clamp(0.05, 16.0);
        let (w, h) = (
            (pw * scale).round().clamp(1.0, MAX_SIDE),
            (ph * scale).round().clamp(1.0, MAX_SIDE),
        );
        #[allow(clippy::cast_possible_truncation)]
        let (w, h) = (w as i32, h as i32);
        if i64::from(w) * i64::from(h) > MAX_PIXELS {
            return Err("the page is too big to draw at this zoom".to_owned());
        }
        let guard = self
            .slot
            .lock()
            .map_err(|_| "the pdf engine broke".to_owned())?;
        let engine = guard.as_ref().ok_or("no pdf engine")?;
        let c = &engine.calls;
        let doc = self.doc as Doc;
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let index = index as i32;
        // SAFETY: the document is open while the book lives; the page and the
        // bitmap are made, used and let go inside this block, and the pixels
        // are copied out row by row within the stride pdfium reports.
        unsafe {
            let page = (c.load_page)(doc, index);
            if page.is_null() {
                return Err("the page could not be read".to_owned());
            }
            let bitmap = (c.bitmap_new)(w, h, FPDF_BITMAP_BGRA, std::ptr::null_mut(), 0);
            if bitmap.is_null() {
                (c.close_page)(page);
                return Err("no memory for the page".to_owned());
            }
            (c.bitmap_fill)(bitmap, 0, 0, w, h, 0xFFFF_FFFF);
            // reversed byte order: rgba, as aurea takes it
            (c.render)(
                bitmap,
                page,
                0,
                0,
                w,
                h,
                0,
                FPDF_ANNOT | FPDF_REVERSE_BYTE_ORDER,
            );
            let stride = (c.bitmap_stride)(bitmap);
            let buffer = (c.bitmap_buffer)(bitmap).cast::<u8>();
            let row = w as usize * 4;
            let mut rgba = Vec::with_capacity(row * h as usize);
            if !buffer.is_null() && stride as usize >= row {
                for y in 0..h as usize {
                    let line = std::slice::from_raw_parts(buffer.add(y * stride as usize), row);
                    rgba.extend_from_slice(line);
                }
            }
            (c.bitmap_drop)(bitmap);
            (c.close_page)(page);
            if rgba.len() != row * h as usize {
                return Err("the page drew nothing".to_owned());
            }
            #[allow(clippy::cast_sign_loss)]
            Ok(Image::new(w as u32, h as u32, rgba))
        }
    }
}

impl Drop for Book {
    fn drop(&mut self) {
        if let Ok(guard) = self.slot.lock()
            && let Some(engine) = guard.as_ref()
        {
            // SAFETY: the document was opened by this engine and closes once.
            unsafe { (engine.calls.close_doc)(self.doc as Doc) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// one blue box on a 200 by 100 page.
    const SMALL: &[u8] = b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]/Contents 4 0 R>>endobj\n4 0 obj<</Length 27>>stream\n0 0 1 rg 20 20 160 60 re f\nendstream endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n";

    /// these run where pdfium is found (LIGASE_PDFIUM in a dev shell); else
    /// they say so and pass.
    fn slot() -> Option<Slot> {
        let slot: Slot = Arc::default();
        match Engine::load() {
            Ok(e) => {
                *slot.lock().ok()? = Some(e);
                Some(slot)
            }
            Err(why) => {
                eprintln!("skipped: {why}");
                None
            }
        }
    }

    #[test]
    fn a_page_draws_where_its_box_is() {
        let Some(slot) = slot() else { return };
        let book = open(&slot, Arc::from(SMALL)).expect("opens");
        assert_eq!(book.pages, vec![(200.0, 100.0)]);
        let image = book.render(0, 1.0).expect("draws");
        assert_eq!((image.width, image.height), (200, 100));
        let at = |x: usize, y: usize| {
            let i = (y * 200 + x) * 4;
            (image.data[i], image.data[i + 1], image.data[i + 2])
        };
        assert_eq!(at(100, 50), (0, 0, 255), "blue in the box");
        assert_eq!(at(5, 5), (255, 255, 255), "white around it");
    }

    #[test]
    fn junk_and_cut_files_say_why() {
        let Some(slot) = slot() else { return };
        for bytes in [&b"not a pdf"[..], &SMALL[..40], b""] {
            assert!(open(&slot, Arc::from(bytes)).is_err());
        }
    }
}
