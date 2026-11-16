//! Small parallel helper for per-marker work (decode, view data): several threads, results in
//! input order, so the output is the same as a plain loop.

use std::sync::atomic::{AtomicUsize, Ordering};

const EXTRA_HELPERS: usize = 7;

/// Separate process-wide budgets: callers always keep working, even when all helpers are busy.
struct HelperBudget(AtomicUsize);
impl HelperBudget {
    const fn new() -> Self { Self(AtomicUsize::new(0)) }

    fn reserve(&self, wanted: usize) -> Helpers<'_> {
        let mut used = self.0.load(Ordering::Relaxed);
        loop {
            let count = wanted.min(EXTRA_HELPERS.saturating_sub(used));
            if count == 0 { return Helpers { budget: self, count: 0 }; }
            match self.0.compare_exchange_weak(used, used + count, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => return Helpers { budget: self, count },
                Err(now) => used = now,
            }
        }
    }
}

pub(crate) struct Helpers<'a> { budget: &'a HelperBudget, count: usize }
impl Helpers<'_> {
    pub(crate) fn len(&self) -> usize { self.count }
}
impl Drop for Helpers<'_> {
    fn drop(&mut self) { self.budget.0.fetch_sub(self.count, Ordering::Release); }
}

static CPU_HELPERS: HelperBudget = HelperBudget::new();
static IO_HELPERS: HelperBudget = HelperBudget::new();

pub(crate) fn io_helpers(wanted: usize) -> Helpers<'static> { IO_HELPERS.reserve(wanted) }

/// Threads for CPU work: the machine's cores, at most 8.
fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get()).min(8)
}

/// `items.iter().map(f).collect()`, spread over threads. Items are handed out one at a time,
/// so a few long trajectories do not leave the other threads idle.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    par_map_with_budget(items, f, &CPU_HELPERS, threads())
}

fn par_map_with_budget<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync, budget: &HelperBudget, width: usize) -> Vec<R> {
    let helpers = budget.reserve(width.min(items.len()).saturating_sub(1));
    if helpers.len() == 0 {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let work = || {
        let mut out = Vec::new();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(i) else { break };
            out.push((i, f(item)));
        }
        out
    };
    let mut parts: Vec<Vec<(usize, R)>> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..helpers.len()).map(|_| s.spawn(&work)).collect();
        let mut parts = vec![work()];
        parts.extend(workers.into_iter().map(|w| w.join().expect("worker thread panicked")));
        parts
    });
    drop(helpers);
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
    use std::sync::{Barrier, Mutex};

    #[test]
    fn same_as_plain_map_in_order() {
        let items: Vec<u64> = (0..1000).collect();
        let f = |x: &u64| x * x + 1;
        assert_eq!(par_map(&items, f), items.iter().map(f).collect::<Vec<_>>());
        assert_eq!(par_map(&[] as &[u64], f), Vec::<u64>::new());
        assert_eq!(par_map(&[3u64], f), vec![10]);
    }

    #[test]
    fn concurrent_callers_share_seven_extra_helpers() {
        let budget = HelperBudget::new();
        let ready = Barrier::new(13);
        let release = Barrier::new(13);
        let (active, counts) = std::thread::scope(|s| {
            let callers: Vec<_> = (0..12).map(|_| s.spawn(|| {
                let helpers = budget.reserve(7);
                ready.wait();
                release.wait();
                helpers.len()
            })).collect();
            ready.wait();
            let active = budget.0.load(Ordering::Relaxed);
            release.wait();
            (active, callers.into_iter().map(|t| t.join().unwrap()).collect::<Vec<_>>())
        });
        assert_eq!(active, 7);
        assert_eq!(counts.iter().sum::<usize>(), 7);
        assert_eq!(budget.0.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn exhausted_budget_returns_without_waiting() {
        let budget = HelperBudget::new();
        let held = budget.reserve(7);
        let result = std::thread::scope(|s| {
            let (tx, rx) = std::sync::mpsc::channel();
            let budget = &budget;
            s.spawn(move || { tx.send(budget.reserve(1).len()).unwrap(); });
            let result = rx.recv_timeout(std::time::Duration::from_secs(2));
            // Release before asserting, so a broken blocking implementation cannot strand
            // the scoped worker during failure cleanup.
            drop(held);
            result
        });
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn single_file_uses_caller_and_seven_helpers_and_preserves_order() {
        let budget = HelperBudget::new();
        let all_working = Barrier::new(8);
        let ids = Mutex::new(std::collections::HashSet::new());
        let caller = std::thread::current().id();
        let items: Vec<usize> = (0..8).collect();
        let result = par_map_with_budget(&items, |n| {
            ids.lock().unwrap().insert(std::thread::current().id());
            all_working.wait();
            n * n
        }, &budget, 8);
        assert_eq!(result, items.iter().map(|n| n * n).collect::<Vec<_>>());
        let ids = ids.into_inner().unwrap();
        assert_eq!(ids.len(), 8);
        assert!(ids.contains(&caller));
        assert_eq!(budget.0.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn saturated_mapping_stays_on_caller() {
        let budget = HelperBudget::new();
        let held = budget.reserve(7);
        let caller = std::thread::current().id();
        let items: Vec<usize> = (0..32).collect();
        let result = par_map_with_budget(&items, |n| {
            assert_eq!(std::thread::current().id(), caller);
            n + 1
        }, &budget, 8);
        assert_eq!(result, (1..33).collect::<Vec<_>>());
        drop(held);
        assert_eq!(budget.0.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn caller_and_helper_unwind_both_return_permits() {
        let budget = HelperBudget::new();
        for panic_on_caller in [true, false] {
            let all_working = Barrier::new(8);
            let caller = std::thread::current().id();
            let items: Vec<usize> = (0..8).collect();
            let failure = std::panic::catch_unwind(|| {
                par_map_with_budget(&items, |n| {
                    all_working.wait();
                    if (std::thread::current().id() == caller) == panic_on_caller {
                        panic!("mapping failure");
                    }
                    n + 1
                }, &budget, 8)
            });
            assert!(failure.is_err());
            assert_eq!(budget.0.load(Ordering::Relaxed), 0);
            assert_eq!(budget.reserve(7).len(), 7);
        }
    }
}
