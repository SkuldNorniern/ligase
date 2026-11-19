//! file work off the ui thread: one worker runs jobs in turn and wakes the
//! canvas when one is done, so the app picks the result up next frame.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use aurea::render::{CanvasId, request_canvas_redraw};

type Job<T> = Box<dyn FnOnce() -> T + Send>;

/// a worker for jobs that each give a `T`.
pub struct Jobs<T: Send + 'static> {
    to_worker: Option<Sender<Job<T>>>,
    done: Receiver<T>,
    canvas: Arc<Mutex<Option<CanvasId>>>,
}

impl<T: Send + 'static> Default for Jobs<T> {
    fn default() -> Self {
        let (to_worker, jobs) = channel::<Job<T>>();
        let (results, done) = channel();
        let canvas: Arc<Mutex<Option<CanvasId>>> = Arc::default();
        let wake = Arc::clone(&canvas);
        let started = std::thread::Builder::new()
            .name("ligase-files".into())
            .spawn(move || {
                for job in jobs {
                    if results.send(job()).is_err() {
                        return;
                    }
                    if let Some(id) = wake.lock().ok().and_then(|c| *c) {
                        request_canvas_redraw(id);
                    }
                }
            })
            .inspect_err(|e| eprintln!("ligase: can't start the file worker: {e}"));
        Self {
            to_worker: started.ok().map(|_| to_worker),
            done,
            canvas,
        }
    }
}

impl<T: Send + 'static> Jobs<T> {
    /// the canvas to wake when a job is done.
    pub fn set_canvas(&mut self, canvas: CanvasId) {
        if let Ok(mut c) = self.canvas.lock() {
            *c = Some(canvas);
        }
    }

    /// runs `job` after the ones before it. false when there is no worker.
    pub fn run(&mut self, job: impl FnOnce() -> T + Send + 'static) -> bool {
        self.to_worker
            .as_ref()
            .is_some_and(|tx| tx.send(Box::new(job)).is_ok())
    }

    /// a finished job's result, if one is in. the worker wakes the canvas
    /// for each one, so nobody has to keep drawing to wait for it.
    pub fn take(&mut self) -> Option<T> {
        self.done.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::Jobs;

    #[test]
    fn jobs_come_back_in_order() {
        let mut jobs = Jobs::default();
        assert!(jobs.run(|| 1));
        assert!(jobs.run(|| 2));
        let mut got = Vec::new();
        while got.len() < 2 {
            if let Some(n) = jobs.take() {
                got.push(n);
            }
        }
        assert_eq!(got, [1, 2]);
        assert!(jobs.take().is_none());
    }
}
