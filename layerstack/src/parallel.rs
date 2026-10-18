// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A pool of scoped worker threads for tree-shaped work: each task may add
//! more, and a phase ends when no task is left. Composition indexes prims
//! on one ([`crate::StageOptions::threads`]), and a host may read layers on
//! one.
//!
//! With the `std` feature a pool of `n` threads works on the calling thread
//! during each [`Pool::run`], and starts up to `n - 1` workers, one each
//! time tasks wait with none idle, which last as long as [`with_pool`]
//! runs: a phase with one task at a time starts none. Each thread does the
//! tasks a task adds itself, depth first, and hands the rest of its
//! pending tasks to the pool only when another thread waits for one, so
//! threads take the pool's lock about once per subtree they take rather
//! than once per task. Without `std`, or with one thread, tasks run on the
//! calling thread, depth first. Either way a phase returns every task's
//! result; their order is not fixed when workers run.
//!
//! OpenUSD composes subtrees on a `WorkDispatcher`
//! (`UsdStage::_ComposeSubtreesInParallel`).

use alloc::vec::Vec;

/// What a pool does with each task: returns its result and pushes the
/// tasks it adds. Shared by every worker, so [`Sync`] with `std`.
#[cfg(feature = "std")]
pub type Work<'w, T, R> = dyn Fn(T, &mut Vec<T>) -> R + Sync + 'w;
/// What a pool does with each task: returns its result and pushes the
/// tasks it adds.
#[cfg(not(feature = "std"))]
pub type Work<'w, T, R> = dyn Fn(T, &mut Vec<T>) -> R + 'w;

/// Runs `body` with a pool of up to `threads` threads doing `work`.
///
/// ```
/// use layerstack::parallel::with_pool;
///
/// // Each task `n` adds `n - 1` twice, down to 0.
/// let tree = |n: u32, added: &mut Vec<u32>| {
///     if n > 0 {
///         added.extend([n - 1, n - 1]);
///     }
///     n
/// };
/// let results = with_pool(4, &tree, |pool| pool.run(vec![3]));
/// assert_eq!(results.len(), 15);
/// ```
pub fn with_pool<T, R, B>(
    threads: usize,
    work: &Work<'_, T, R>,
    body: impl FnOnce(&Pool<'_, T, R>) -> B,
) -> B
where
    T: Send,
    R: Send,
{
    #[cfg(feature = "std")]
    if threads > 1 {
        let threaded = Threaded {
            shared: std::sync::Mutex::new(Shared {
                pending: Vec::new(),
                busy: 0,
                workers: 0,
                results: Vec::new(),
                done: false,
            }),
            changed: std::sync::Condvar::new(),
            max_workers: threads - 1,
            waiting: core::sync::atomic::AtomicUsize::new(0),
        };
        return std::thread::scope(|scope| {
            let spawner = Spawner {
                scope,
                work,
                threaded: &threaded,
            };
            let pool = Pool {
                work,
                threaded: Some((&threaded, &spawner)),
            };
            let out = body(&pool);
            lock(&threaded.shared).done = true;
            threaded.changed.notify_all();
            out
        });
    }
    let _ = threads;
    body(&Pool {
        work,
        #[cfg(feature = "std")]
        threaded: None,
    })
}

/// A pool of threads doing one kind of task (see [`with_pool`]).
pub struct Pool<'s, T, R> {
    work: &'s Work<'s, T, R>,
    #[cfg(feature = "std")]
    threaded: Option<(&'s Threaded<T, R>, &'s dyn Spawn)>,
}

/// Starts a worker for the pool.
#[cfg(feature = "std")]
trait Spawn {
    /// Starts a worker; `false` where threads cannot start (as on
    /// `wasm32-wasip1`), leaving the tasks to the calling thread.
    fn spawn_worker(&self) -> bool;
}

#[cfg(feature = "std")]
struct Spawner<'s, 'e, T, R> {
    scope: &'s std::thread::Scope<'s, 'e>,
    work: &'e Work<'e, T, R>,
    threaded: &'e Threaded<T, R>,
}

#[cfg(feature = "std")]
impl<T: Send, R: Send> Spawn for Spawner<'_, '_, T, R> {
    fn spawn_worker(&self) -> bool {
        let (work, threaded) = (self.work, self.threaded);
        std::thread::Builder::new()
            .spawn_scoped(self.scope, move || serve(work, threaded))
            .is_ok()
    }
}

impl<T, R> core::fmt::Debug for Pool<'_, T, R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        #[cfg(feature = "std")]
        let threads = self
            .threaded
            .map_or(1, |(threaded, _)| threaded.max_workers + 1);
        #[cfg(not(feature = "std"))]
        let threads = 1;
        f.debug_struct("Pool")
            .field("threads", &threads)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "std")]
struct Threaded<T, R> {
    shared: std::sync::Mutex<Shared<T, R>>,
    /// Signalled when tasks are added, when a phase's last task finishes
    /// and when the pool shuts down.
    changed: std::sync::Condvar,
    max_workers: usize,
    /// Threads waiting for a task, read without the lock: a busy thread
    /// takes the lock to hand over tasks only while one waits.
    waiting: core::sync::atomic::AtomicUsize,
}

#[cfg(feature = "std")]
struct Shared<T, R> {
    pending: Vec<T>,
    /// Tasks taken but not finished.
    busy: usize,
    /// Workers started, or the most when one failed to start.
    workers: usize,
    results: Vec<R>,
    done: bool,
}

impl<T: Send, R: Send> Pool<'_, T, R> {
    /// Whether [`Self::run`] returns results in the order a single thread
    /// does tasks: depth first, each task's added tasks in order before
    /// the next.
    pub fn in_order(&self) -> bool {
        #[cfg(feature = "std")]
        let in_order = self.threaded.is_none();
        #[cfg(not(feature = "std"))]
        let in_order = true;
        in_order
    }

    /// Does `tasks` and every task they add, and returns their results.
    pub fn run(&self, tasks: Vec<T>) -> Vec<R> {
        #[cfg(feature = "std")]
        if let Some((threaded, spawner)) = self.threaded {
            let mut shared = lock(&threaded.shared);
            shared.pending.extend(tasks.into_iter().rev());
            threaded.changed.notify_all();
            loop {
                if !shared.pending.is_empty() {
                    shared = work_on(self.work, threaded, Some(spawner), shared);
                } else if shared.busy == 0 {
                    return core::mem::take(&mut shared.results);
                } else {
                    // Waiting for a task as much as for the phase to end.
                    let waiting = &threaded.waiting;
                    waiting.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    shared = wait(threaded, shared);
                    waiting.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        let mut pending = tasks;
        pending.reverse();
        let mut results = Vec::new();
        let mut added = Vec::new();
        while let Some(task) = pending.pop() {
            results.push((self.work)(task, &mut added));
            pending.extend(added.drain(..).rev());
        }
        results
    }
}

#[cfg(feature = "std")]
fn lock<S>(mutex: &std::sync::Mutex<S>) -> std::sync::MutexGuard<'_, S> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(feature = "std")]
fn wait<'s, T, R>(
    threaded: &'s Threaded<T, R>,
    shared: std::sync::MutexGuard<'s, Shared<T, R>>,
) -> std::sync::MutexGuard<'s, Shared<T, R>> {
    threaded
        .changed
        .wait(shared)
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Takes a share of the pool's tasks, at least one, and does them and the
/// tasks they add without the lock, depth first, then records their
/// results. Pending tasks go back to the pool while a thread waits for
/// one, all but the next: the oldest, nearest the subtree's root, hold the
/// most work. `spawner`, the calling thread's, starts a worker to take
/// them when none waits and the pool has room for one.
#[cfg(feature = "std")]
fn work_on<'s, T, R>(
    work: &Work<'_, T, R>,
    threaded: &'s Threaded<T, R>,
    spawner: Option<&dyn Spawn>,
    mut shared: std::sync::MutexGuard<'s, Shared<T, R>>,
) -> std::sync::MutexGuard<'s, Shared<T, R>> {
    use core::sync::atomic::Ordering;
    // An even share for each thread, so many small tasks, as a wide
    // parent's childless children are, cost one lock each per share.
    let len = shared.pending.len();
    let share = (len / (threaded.max_workers + 1)).max(1);
    let mut local = shared.pending.split_off(len - share);
    shared.busy += 1;
    let mut can_spawn = spawner.is_some() && shared.workers < threaded.max_workers;
    drop(shared);
    let mut results = Vec::new();
    let mut added = Vec::new();
    while let Some(task) = local.pop() {
        results.push(work(task, &mut added));
        local.extend(added.drain(..).rev());
        if local.len() < 2 {
            continue;
        }
        let waiting = threaded.waiting.load(Ordering::Relaxed);
        if waiting == 0 && !can_spawn {
            continue;
        }
        let mut shared = lock(&threaded.shared);
        if waiting == 0
            && let Some(spawner) = spawner
        {
            // Tasks wait with no thread idle to take them: start one.
            shared.workers = if spawner.spawn_worker() {
                shared.workers + 1
            } else {
                threaded.max_workers
            };
            can_spawn = shared.workers < threaded.max_workers;
        }
        let next = local.pop().expect("two tasks pending");
        let handed = local.len();
        // The pool's tasks are taken from its end: the oldest go there.
        shared.pending.extend(local.drain(..).rev());
        local.push(next);
        drop(shared);
        if handed >= threaded.waiting.load(Ordering::Relaxed) {
            threaded.changed.notify_all();
        } else {
            for _ in 0..handed {
                threaded.changed.notify_one();
            }
        }
    }
    let mut shared = lock(&threaded.shared);
    shared.busy -= 1;
    shared.results.append(&mut results);
    if shared.busy == 0 && shared.pending.is_empty() {
        threaded.changed.notify_all();
    }
    shared
}

/// A worker's loop: does tasks as they come until the pool shuts down.
#[cfg(feature = "std")]
fn serve<T, R>(work: &Work<'_, T, R>, threaded: &Threaded<T, R>) {
    use core::sync::atomic::Ordering;
    let mut shared = lock(&threaded.shared);
    loop {
        if !shared.pending.is_empty() {
            shared = work_on(work, threaded, None, shared);
        } else if shared.done {
            return;
        } else {
            threaded.waiting.fetch_add(1, Ordering::Relaxed);
            shared = wait(threaded, shared);
            threaded.waiting.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each task `n` adds `n - 1` twice, down to 0: `2^(n+1) - 1` tasks.
    fn tree(task: u32, added: &mut Vec<u32>) -> u32 {
        if task > 0 {
            added.extend([task - 1, task - 1]);
        }
        task
    }

    #[test]
    fn one_thread_runs_tasks_depth_first() {
        let order = with_pool(1, &tree, |pool| pool.run(alloc::vec![2]));
        assert_eq!(order, [2, 1, 0, 0, 1, 0, 0]);
    }

    #[test]
    fn every_task_runs_once_on_any_number_of_threads() {
        for threads in [1, 2, 8] {
            let results = with_pool(threads, &tree, |pool| {
                let mut first = pool.run(alloc::vec![10, 3]);
                // A second phase on the same pool.
                first.extend(pool.run(alloc::vec![4]));
                first
            });
            assert_eq!(results.len(), (2 << 10) - 1 + (2 << 3) - 1 + (2 << 4) - 1);
            let zeros = results.iter().filter(|&&n| n == 0).count();
            assert_eq!(zeros, (1 << 10) + (1 << 3) + (1 << 4));
        }
    }
}
