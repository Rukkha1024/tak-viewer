//! Small persistent queues: CPU geometry preparation and retirement never spawn per frame.
use std::sync::{mpsc, Arc, Mutex, OnceLock};
type Job = Box<dyn FnOnce() + Send + 'static>;
fn queue(threads: usize) -> mpsc::Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..threads {
        let rx = rx.clone();
        std::thread::spawn(move || loop {
            let job = rx.lock().unwrap().recv();
            match job {
                Ok(job) => job(),
                Err(_) => break,
            }
        });
    }
    tx
}
pub fn geometry(job: impl FnOnce() + Send + 'static) {
    static QUEUE: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    let _ = QUEUE.get_or_init(|| queue(4)).send(Box::new(job));
}
pub fn retire<T: Send + 'static>(value: T) {
    static QUEUE: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    let _ = QUEUE
        .get_or_init(|| queue(1))
        .send(Box::new(move || drop(value)));
}

/// Bounds a test's wait for a specific worker result. Report the last state on timeout.
#[cfg(test)]
pub(crate) struct TestWatchdog(std::time::Instant);

#[cfg(test)]
impl TestWatchdog {
    pub(crate) fn new() -> Self { Self(std::time::Instant::now()) }

    pub(crate) fn pause(&self, pending: impl FnOnce() -> String) {
        let elapsed = self.0.elapsed();
        if elapsed >= std::time::Duration::from_secs(60) {
            panic!("still waiting after {elapsed:?}: {}", pending());
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
