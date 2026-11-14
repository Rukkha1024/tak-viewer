//! Small parallel helper for per-marker work (decode, view data): several threads, results in
//! input order, so the output is the same as a plain loop.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Threads for CPU work: the machine's cores, at most 8.
fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get()).min(8)
}

/// `items.iter().map(f).collect()`, spread over threads. Items are handed out one at a time,
/// so a few long trajectories do not leave the other threads idle.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = threads().min(items.len());
    if threads < 2 {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let mut parts: Vec<Vec<(usize, R)>> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        out.push((i, f(item)));
                    }
                    out
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().expect("worker thread panicked")).collect()
    });
    let mut slots: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    for part in parts.iter_mut() {
        for (i, r) in part.drain(..) {
            slots[i] = Some(r);
        }
    }
    slots.into_iter().map(|r| r.expect("every item mapped")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_as_plain_map_in_order() {
        let items: Vec<u64> = (0..1000).collect();
        let f = |x: &u64| x * x + 1;
        assert_eq!(par_map(&items, f), items.iter().map(f).collect::<Vec<_>>());
        assert_eq!(par_map(&[] as &[u64], f), Vec::<u64>::new());
        assert_eq!(par_map(&[3u64], f), vec![10]);
    }
}
