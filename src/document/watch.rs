//! notices open files changing on disk while the window has the focus too:
//! a thread looks at their stamps every so often and wakes the canvas only
//! when one moved. the app then checks them the usual way.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aurea::render::{CanvasId, request_canvas_redraw};

use super::{DocumentId, Stamp, stamp};

/// how often the open files are looked at.
const EVERY: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Shared {
    /// the files to watch, as the app last said.
    files: Mutex<Vec<(DocumentId, PathBuf, Option<Stamp>)>>,
    canvas: Mutex<Option<CanvasId>>,
    changed: AtomicBool,
}

pub struct Watcher {
    shared: Arc<Shared>,
}

impl Default for Watcher {
    fn default() -> Self {
        let shared = Arc::new(Shared::default());
        let weak = Arc::downgrade(&shared);
        let _ = std::thread::Builder::new()
            .name("ligase-watch".into())
            .spawn(move || {
                // the stamp each path had when last looked at
                let mut seen: HashMap<PathBuf, Option<Stamp>> = HashMap::new();
                loop {
                    std::thread::sleep(EVERY);
                    // the app is gone: so is the watcher
                    let Some(shared) = weak.upgrade() else {
                        return;
                    };
                    let files = shared.files.lock().map(|f| f.clone()).unwrap_or_default();
                    let mut moved = false;
                    for (_, path, was) in &files {
                        let now = stamp(path);
                        let last = seen.entry(path.clone()).or_insert(*was);
                        if *last != now {
                            *last = now;
                            moved = true;
                        }
                    }
                    seen.retain(|p, _| files.iter().any(|(_, f, _)| f == p));
                    if moved {
                        shared.changed.store(true, Ordering::Relaxed);
                        if let Some(id) = shared.canvas.lock().ok().and_then(|c| *c) {
                            request_canvas_redraw(id);
                        }
                    }
                }
            });
        Self { shared }
    }
}

impl Watcher {
    pub fn set_canvas(&self, canvas: CanvasId) {
        if let Ok(mut c) = self.shared.canvas.lock() {
            *c = Some(canvas);
        }
    }

    /// the files to watch now, with the stamp each had when read or saved.
    pub fn watch(&self, files: Vec<(DocumentId, PathBuf, Option<Stamp>)>) {
        if let Ok(mut f) = self.shared.files.lock()
            && *f != files
        {
            *f = files;
        }
    }

    /// whether a watched file moved since this was last asked.
    pub fn take_changed(&self) -> bool {
        self.shared.changed.swap(false, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_changed_outside_is_noticed_once() {
        let dir = std::env::temp_dir().join(format!("ligase-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one").unwrap();
        let watcher = Watcher::default();
        let (id, _) = crate::document::Documents::default().open(&file);
        watcher.watch(vec![(id, file.clone(), stamp(&file))]);
        std::thread::sleep(EVERY + Duration::from_millis(500));
        assert!(!watcher.take_changed(), "nothing moved yet");
        std::fs::write(&file, "two, longer").unwrap();
        std::thread::sleep(EVERY + Duration::from_millis(500));
        assert!(watcher.take_changed());
        assert!(!watcher.take_changed(), "told once");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
