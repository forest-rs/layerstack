// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A pool of scoped worker threads for tree-shaped work: each task may add
//! more, and a phase ends when no task is left. Composition indexes prims
//! on one ([`crate::StageOptions::threads`]), and a host may read layers on
//! one.
//!
//! Pools draw their threads from a budget ([`Threads`]): the calling thread,
//! and as many workers as the budget spares. With the `std` feature a pool
//! works on the calling thread during each [`Pool::run`], and starts a
//! worker each time tasks wait with no thread idle to take them, up to the
//! budget's limit; workers last as long as [`with_pool`] runs, and a phase
//! with one task at a time starts none. A worker with no task gives its
//! thread back to the budget until it has one again, so a task that runs a
//! pool of its own on the same budget, as reading one layer of a stage
//! opened on threads does, can use the threads the outer pool is not using,
//! and the two never run more threads than the budget holds.
//!
//! Each thread does the tasks a task adds itself, depth first, and hands
//! the rest of its pending tasks to the pool only when another thread waits
//! for one, so threads take the pool's lock about once per subtree they
//! take rather than once per task. Without `std`, or with one thread, tasks
//! run on the calling thread, depth first. Either way a phase returns every
//! task's result; their order is not fixed when workers run.
//!
//! OpenUSD composes subtrees on a `WorkDispatcher`
//! (`UsdStage::_ComposeSubtreesInParallel`), and its dispatchers share one
//! thread pool, sized by `WorkSetConcurrencyLimit`.

use alloc::vec::Vec;
use core::fmt;
#[cfg(feature = "std")]
use core::sync::atomic::{AtomicUsize, Ordering};

/// The threads that work may run on at once, the calling thread among
/// them: a budget that every pool started under it shares ([`with_pool_in`]).
///
/// A pool starts a worker only when the budget has a thread to spare, and
/// a worker gives its thread back while it waits for a task. So a pool run
/// from a task of another pool on the same budget runs on the threads the
/// other is not using at the time, and the two together never run more
/// than [`Self::limit`] threads. Without the `std` feature every pool runs
/// on the calling thread.
pub struct Threads {
    limit: usize,
    /// The threads beyond the calling thread that no worker holds.
    #[cfg(feature = "std")]
    spare: AtomicUsize,
}

impl Threads {
    /// A budget of `limit` threads, the calling thread among them; `0` and
    /// `1` run every pool on the calling thread.
    pub const fn new(limit: usize) -> Self {
        let limit = if limit == 0 { 1 } else { limit };
        Self {
            limit,
            #[cfg(feature = "std")]
            spare: AtomicUsize::new(limit - 1),
        }
    }

    /// The most threads work under this budget runs on at once.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The threads beyond the calling thread that no worker holds now: how
    /// many workers a pool could start at this moment.
    pub fn spare(&self) -> usize {
        #[cfg(feature = "std")]
        let spare = self.spare.load(Ordering::Relaxed);
        #[cfg(not(feature = "std"))]
        let spare = 0;
        spare
    }

    /// Takes a spare thread for a worker, if there is one.
    #[cfg(feature = "std")]
    fn claim(&self) -> bool {
        let mut spare = self.spare.load(Ordering::Relaxed);
        while spare > 0 {
            match self.spare.compare_exchange_weak(
                spare,
                spare - 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(now) => spare = now,
            }
        }
        false
    }

    /// Gives back a thread [`Self::claim`] took.
    #[cfg(feature = "std")]
    fn give_back(&self) {
        self.spare.fetch_add(1, Ordering::Release);
    }
}

impl fmt::Debug for Threads {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Threads")
            .field("limit", &self.limit)
            .field("spare", &self.spare())
            .finish()
    }
}

/// What a pool does with each task: returns its result and pushes the
/// tasks it adds. Shared by every worker, so [`Sync`] with `std`.
#[cfg(feature = "std")]
pub type Work<'w, T, R> = dyn Fn(T, &mut Vec<T>) -> R + Sync + 'w;
/// What a pool does with each task: returns its result and pushes the
/// tasks it adds.
#[cfg(not(feature = "std"))]
pub type Work<'w, T, R> = dyn Fn(T, &mut Vec<T>) -> R + 'w;

/// What a pool started with [`with_offering_pool_in`] does with each task:
/// returns its result and adds tasks to [`Added`], which may hand them to
/// the pool before the task returns. Shared by every worker, so [`Sync`]
/// with `std`.
#[cfg(feature = "std")]
pub type Offering<'w, T, R> = dyn Fn(T, &mut Added<'_, T>) -> R + Sync + 'w;
/// What a pool started with [`with_offering_pool_in`] does with each task:
/// returns its result and adds tasks to [`Added`].
#[cfg(not(feature = "std"))]
pub type Offering<'w, T, R> = dyn Fn(T, &mut Added<'_, T>) -> R + 'w;

/// The tasks a task of an offering pool adds ([`with_offering_pool_in`]):
/// done by the thread that added them, depth first, after the task
/// returns, unless [`Self::offer`] hands them to the pool sooner.
pub struct Added<'a, T> {
    tasks: &'a mut Vec<T>,
    #[cfg(feature = "std")]
    pool: Option<&'a dyn Offer<T>>,
}

impl<T> Added<'_, T> {
    /// Adds `task`.
    pub fn push(&mut self, task: T) {
        self.tasks.push(task);
    }

    /// Hands the tasks added so far to the pool now, if a thread is idle to
    /// take them or the pool can start one, so that they need not wait for
    /// the rest of the task: a task that finds the tasks it adds while it
    /// runs, as reading a layer finds the layers its arcs name, offers each
    /// as it finds it. Does nothing on one thread.
    pub fn offer(&mut self) {
        #[cfg(feature = "std")]
        if let Some(pool) = self.pool
            && !self.tasks.is_empty()
        {
            pool.offer(self.tasks);
        }
    }
}

impl<T> fmt::Debug for Added<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Added")
            .field("tasks", &self.tasks.len())
            .finish_non_exhaustive()
    }
}

/// Hands tasks to a pool from inside one of its tasks ([`Added::offer`]).
#[cfg(feature = "std")]
trait Offer<T> {
    fn offer(&self, tasks: &mut Vec<T>);
}

/// A pool's work, either kind.
enum Job<'w, T, R> {
    Plain(&'w Work<'w, T, R>),
    Offering(&'w Offering<'w, T, R>),
}

impl<T, R> Clone for Job<'_, T, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, R> Copy for Job<'_, T, R> {}

impl<T, R> Job<'_, T, R> {
    /// Does `task`, adding the tasks it adds to `added`; an offering task
    /// may hand them to `pool` first.
    fn call(
        self,
        task: T,
        added: &mut Vec<T>,
        #[cfg(feature = "std")] pool: Option<&dyn Offer<T>>,
    ) -> R {
        match self {
            Self::Plain(work) => work(task, added),
            Self::Offering(work) => work(
                task,
                &mut Added {
                    tasks: added,
                    #[cfg(feature = "std")]
                    pool,
                },
            ),
        }
    }
}

/// Runs `body` with a pool of up to `threads` threads doing `work`, on a
/// budget of its own ([`with_pool_in`]).
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
    with_pool_in(&Threads::new(threads), work, body)
}

/// Runs `body` with a pool doing `work` on the threads of `threads`: the
/// calling thread, and the workers the budget spares while the pool runs.
///
/// ```
/// use layerstack::parallel::{Threads, with_pool_in};
///
/// let threads = Threads::new(4);
/// // Each task sums a range on a pool of its own on the same budget,
/// // which runs on the threads the outer pool leaves idle.
/// let inner = |n: u64, _: &mut Vec<u64>| n;
/// let outer = |n: u64, _: &mut Vec<u64>| -> u64 {
///     with_pool_in(&threads, &inner, |pool| pool.run((0..n).collect()))
///         .into_iter()
///         .sum()
/// };
/// let sums = with_pool_in(&threads, &outer, |pool| pool.run(vec![10, 100]));
/// assert_eq!(sums.iter().sum::<u64>(), 45 + 4950);
/// assert_eq!(threads.spare(), 3);
/// ```
pub fn with_pool_in<T, R, B>(
    threads: &Threads,
    work: &Work<'_, T, R>,
    body: impl FnOnce(&Pool<'_, T, R>) -> B,
) -> B
where
    T: Send,
    R: Send,
{
    pool_in(threads, Job::Plain(work), body)
}

/// Runs `body` with a pool doing `work` on the threads of `threads`, as
/// [`with_pool_in`] does, whose tasks may hand the tasks they add to the
/// pool before they return ([`Added::offer`]).
///
/// ```
/// use layerstack::parallel::{Added, Threads, with_offering_pool_in};
///
/// // Task `n` adds `n - 1` twice and offers them at once, down to 0.
/// let tree = |n: u32, added: &mut Added<'_, u32>| {
///     if n > 0 {
///         added.push(n - 1);
///         added.push(n - 1);
///         added.offer();
///     }
///     n
/// };
/// let threads = Threads::new(4);
/// let results = with_offering_pool_in(&threads, &tree, |pool| pool.run(vec![3]));
/// assert_eq!(results.len(), 15);
/// ```
pub fn with_offering_pool_in<T, R, B>(
    threads: &Threads,
    work: &Offering<'_, T, R>,
    body: impl FnOnce(&Pool<'_, T, R>) -> B,
) -> B
where
    T: Send,
    R: Send,
{
    pool_in(threads, Job::Offering(work), body)
}

/// Runs `a` on the calling thread and, at the same time, `b` on a thread
/// the budget `threads` spares, and returns both results. With no thread
/// to spare, or without the `std` feature, `b` runs after `a` on the
/// calling thread. `b` holds its thread until it returns, so work either
/// runs on the same budget ([`with_pool_in`], another `join`) takes the
/// threads left.
///
/// For independent work, whose results do not depend on the thread that
/// does it.
///
/// ```
/// use layerstack::parallel::{Threads, join};
///
/// let threads = Threads::new(2);
/// let (evens, odds) = join(
///     &threads,
///     || (0..10u32).filter(|n| n % 2 == 0).sum::<u32>(),
///     || (0..10u32).filter(|n| n % 2 == 1).sum::<u32>(),
/// );
/// assert_eq!((evens, odds), (20, 25));
/// assert_eq!(threads.spare(), 1);
/// ```
pub fn join<A, B>(threads: &Threads, a: impl FnOnce() -> A, b: impl FnOnce() -> B + Send) -> (A, B)
where
    B: Send,
{
    #[cfg(feature = "std")]
    if threads.claim() {
        // Taken by whichever thread runs it: the one started for it, or
        // this one where threads cannot start (as on `wasm32-wasip1`).
        let task = std::sync::Mutex::new(Some(b));
        let run = || lock(&task).take().map(|b| b());
        let out = std::thread::scope(|scope| {
            let started = std::thread::Builder::new().spawn_scoped(scope, run);
            let a = a();
            let b = match started {
                Ok(handle) => handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                Err(_) => None,
            };
            (a, b)
        });
        threads.give_back();
        let (a, b) = out;
        let b = b.or_else(run).expect("the second task runs once");
        return (a, b);
    }
    let _ = threads;
    (a(), b())
}

fn pool_in<T, R, B>(
    threads: &Threads,
    work: Job<'_, T, R>,
    body: impl FnOnce(&Pool<'_, T, R>) -> B,
) -> B
where
    T: Send,
    R: Send,
{
    #[cfg(feature = "std")]
    if threads.limit > 1 {
        let threaded = Threaded {
            shared: std::sync::Mutex::new(Shared {
                pending: Vec::new(),
                busy: 0,
                workers: 0,
                results: Vec::new(),
                done: false,
            }),
            changed: std::sync::Condvar::new(),
            max_workers: threads.limit - 1,
            budget: threads,
            waiting: AtomicUsize::new(0),
            callers_waiting: AtomicUsize::new(0),
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
    work: Job<'s, T, R>,
    #[cfg(feature = "std")]
    threaded: Option<(&'s Threaded<'s, T, R>, &'s dyn Spawn)>,
}

/// Starts a worker for the pool.
#[cfg(feature = "std")]
trait Spawn {
    /// Starts a worker holding a thread claimed from the budget; `false`
    /// where threads cannot start (as on `wasm32-wasip1`), leaving the
    /// tasks to the calling thread.
    fn spawn_worker(&self) -> bool;
}

/// What starts a worker: copied into each worker, so that a task on any
/// thread can start one for the tasks it offers ([`Added::offer`]).
#[cfg(feature = "std")]
struct Spawner<'s, 'e, T, R> {
    scope: &'s std::thread::Scope<'s, 'e>,
    work: Job<'e, T, R>,
    threaded: &'e Threaded<'e, T, R>,
}

#[cfg(feature = "std")]
impl<T, R> Clone for Spawner<'_, '_, T, R> {
    fn clone(&self) -> Self {
        *self
    }
}

#[cfg(feature = "std")]
impl<T, R> Copy for Spawner<'_, '_, T, R> {}

#[cfg(feature = "std")]
impl<T: Send, R: Send> Spawn for Spawner<'_, '_, T, R> {
    fn spawn_worker(&self) -> bool {
        let spawner = *self;
        std::thread::Builder::new()
            .spawn_scoped(self.scope, move || {
                serve(spawner.work, spawner.threaded, &spawner);
            })
            .is_ok()
    }
}

impl<T, R> fmt::Debug for Pool<'_, T, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
struct Threaded<'b, T, R> {
    shared: std::sync::Mutex<Shared<T, R>>,
    /// Signalled when tasks are added, when a phase's last task finishes
    /// and when the pool shuts down.
    changed: std::sync::Condvar,
    max_workers: usize,
    /// The budget workers claim their threads from.
    budget: &'b Threads,
    /// Workers waiting for a task, read without the lock: each takes one
    /// once it claims a thread from the budget.
    waiting: AtomicUsize,
    /// Callers of [`Pool::run`] waiting for their phase to end, who take a
    /// task on the thread they already run on.
    callers_waiting: AtomicUsize,
}

#[cfg(feature = "std")]
impl<T, R> Threaded<'_, T, R> {
    /// The threads waiting that could take a task now: a busy thread hands
    /// its pending tasks to the pool only while there is one.
    fn takers(&self) -> usize {
        let workers = self.waiting.load(Ordering::Relaxed);
        let workers = if workers == 0 {
            0
        } else {
            workers.min(self.budget.spare())
        };
        self.callers_waiting.load(Ordering::Relaxed) + workers
    }
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
                    shared = work_on(self.work, threaded, spawner, true, shared);
                } else if shared.busy == 0 {
                    return core::mem::take(&mut shared.results);
                } else {
                    // Waiting for a task as much as for the phase to end.
                    let waiting = &threaded.callers_waiting;
                    waiting.fetch_add(1, Ordering::Relaxed);
                    shared = wait(threaded, shared);
                    waiting.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
        let mut pending = tasks;
        pending.reverse();
        let mut results = Vec::new();
        let mut added = Vec::new();
        while let Some(task) = pending.pop() {
            results.push(self.work.call(
                task,
                &mut added,
                #[cfg(feature = "std")]
                None,
            ));
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
    threaded: &'s Threaded<'_, T, R>,
    shared: std::sync::MutexGuard<'s, Shared<T, R>>,
) -> std::sync::MutexGuard<'s, Shared<T, R>> {
    threaded
        .changed
        .wait(shared)
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Starts a worker for tasks waiting with no thread to take them, on a
/// thread claimed from the budget, if the pool and the budget have room.
#[cfg(feature = "std")]
fn spawn_worker<T, R>(
    threaded: &Threaded<'_, T, R>,
    spawner: &dyn Spawn,
    shared: &mut Shared<T, R>,
) {
    if shared.workers >= threaded.max_workers || !threaded.budget.claim() {
        return;
    }
    if spawner.spawn_worker() {
        shared.workers += 1;
    } else {
        threaded.budget.give_back();
        shared.workers = threaded.max_workers;
    }
}

/// Takes a share of the pool's tasks, at least one, and does them and the
/// tasks they add without the lock, depth first, then records their
/// results. Pending tasks go back to the pool while a thread waits for
/// one, all but the next: the oldest, nearest the subtree's root, hold the
/// most work. On the pool's `caller`, `spawner` starts a worker to take
/// them when none waits and the pool and budget have room for one. Tasks
/// offered from a task ([`Added::offer`]) may start one on any thread.
#[cfg(feature = "std")]
fn work_on<'s, T, R>(
    work: Job<'_, T, R>,
    threaded: &'s Threaded<'_, T, R>,
    spawner: &dyn Spawn,
    caller: bool,
    mut shared: std::sync::MutexGuard<'s, Shared<T, R>>,
) -> std::sync::MutexGuard<'s, Shared<T, R>> {
    // An even share for each thread, so many small tasks, as a wide
    // parent's childless children are, cost one lock each per share.
    let len = shared.pending.len();
    let share = (len / (threaded.max_workers + 1)).max(1);
    let mut local = shared.pending.split_off(len - share);
    shared.busy += 1;
    if !shared.pending.is_empty() && threaded.takers() > 0 {
        // Tasks are left for the threads waiting.
        threaded.changed.notify_all();
    }
    let mut can_spawn = caller && shared.workers < threaded.max_workers;
    drop(shared);
    let mut results = Vec::new();
    let mut added = Vec::new();
    let offered = Offered { threaded, spawner };
    while let Some(task) = local.pop() {
        results.push(work.call(task, &mut added, Some(&offered)));
        local.extend(added.drain(..).rev());
        if local.len() < 2 {
            continue;
        }
        let takers = threaded.takers();
        if takers == 0 && !(can_spawn && threaded.budget.spare() > 0) {
            continue;
        }
        let mut shared = lock(&threaded.shared);
        let next = local.pop().expect("two tasks pending");
        let handed = local.len();
        if takers == 0 && can_spawn {
            // Tasks wait with no thread idle to take them: start one.
            spawn_worker(threaded, spawner, &mut shared);
            can_spawn = shared.workers < threaded.max_workers;
        }
        // The pool's tasks are taken from its end: the oldest go there.
        shared.pending.extend(local.drain(..).rev());
        local.push(next);
        drop(shared);
        if handed >= threaded.takers() {
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

/// What a thread doing a pool's task offers the tasks it adds to.
#[cfg(feature = "std")]
struct Offered<'a, 'b, T, R> {
    threaded: &'a Threaded<'b, T, R>,
    spawner: &'a dyn Spawn,
}

#[cfg(feature = "std")]
impl<T, R> Offer<T> for Offered<'_, '_, T, R> {
    /// Moves `tasks` to the pool when a thread waits to take them or the
    /// budget spares one to start, as handing tasks over in [`work_on`]
    /// does.
    fn offer(&self, tasks: &mut Vec<T>) {
        let threaded = self.threaded;
        let takers = threaded.takers();
        if takers == 0 && threaded.budget.spare() == 0 {
            return;
        }
        let mut shared = lock(&threaded.shared);
        if takers == 0 {
            spawn_worker(threaded, self.spawner, &mut shared);
        }
        let handed = tasks.len();
        shared.pending.extend(tasks.drain(..).rev());
        drop(shared);
        if handed >= threaded.takers() {
            threaded.changed.notify_all();
        } else {
            for _ in 0..handed {
                threaded.changed.notify_one();
            }
        }
    }
}

/// A worker's loop: does tasks as they come until the pool shuts down,
/// holding a thread of the budget while it has tasks to do, and giving it
/// back while it waits for them.
#[cfg(feature = "std")]
fn serve<T, R>(work: Job<'_, T, R>, threaded: &Threaded<'_, T, R>, spawner: &dyn Spawn) {
    // Started on a thread claimed for it.
    let mut holds = true;
    let mut shared = lock(&threaded.shared);
    loop {
        if !shared.pending.is_empty() && (holds || threaded.budget.claim()) {
            holds = true;
            shared = work_on(work, threaded, spawner, false, shared);
            continue;
        }
        if holds {
            threaded.budget.give_back();
            holds = false;
        }
        if shared.done {
            return;
        }
        threaded.waiting.fetch_add(1, Ordering::Relaxed);
        shared = wait(threaded, shared);
        threaded.waiting.fetch_sub(1, Ordering::Relaxed);
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

    /// Counts the threads running a task of any pool, however deeply the
    /// pools nest, and the most at once.
    #[cfg(feature = "std")]
    struct Running {
        now: AtomicUsize,
        most: AtomicUsize,
    }

    #[cfg(feature = "std")]
    std::thread_local! {
        /// The pools' tasks this thread is in, outermost first.
        static DEPTH: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
    }

    #[cfg(feature = "std")]
    impl Running {
        /// Runs `f` as a task of a pool, counting this thread while it runs.
        fn task<R>(&self, f: impl FnOnce() -> R) -> R {
            let outermost = DEPTH.with(|depth| depth.replace(depth.get() + 1)) == 0;
            if outermost {
                let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
                self.most.fetch_max(now, Ordering::SeqCst);
            }
            let out = f();
            DEPTH.with(|depth| depth.set(depth.get() - 1));
            if outermost {
                self.now.fetch_sub(1, Ordering::SeqCst);
            }
            out
        }
    }

    /// Spins for about `micros` microseconds, so tasks overlap.
    #[cfg(feature = "std")]
    fn spin(micros: u64) {
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_micros(micros) {
            core::hint::spin_loop();
        }
    }

    /// Pools run from the tasks of a pool on one budget never run more
    /// threads together than the budget holds, and give every thread back
    /// when they end.
    #[cfg(feature = "std")]
    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
    fn nested_pools_never_run_more_threads_than_the_budget() {
        let running = Running {
            now: AtomicUsize::new(0),
            most: AtomicUsize::new(0),
        };
        let threads = Threads::new(3);
        let inner = |n: u32, _: &mut Vec<u32>| {
            running.task(|| {
                spin(20);
                n
            })
        };
        let outer = |n: u32, added: &mut Vec<u32>| {
            running.task(|| {
                if n > 0 {
                    added.extend([n - 1, n - 1]);
                }
                let tasks = (0..40).collect();
                with_pool_in(&threads, &inner, |pool| pool.run(tasks)).len()
            })
        };
        let ran = with_pool_in(&threads, &outer, |pool| pool.run(alloc::vec![3]));
        assert_eq!(ran.iter().sum::<usize>(), 15 * 40);
        assert!(running.most.load(Ordering::SeqCst) <= 3);
        assert_eq!(threads.spare(), 2);
    }

    /// A pool run from a pool's only task takes the threads the outer pool
    /// leaves idle: two of its tasks run at once.
    #[cfg(feature = "std")]
    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
    fn a_nested_pool_takes_the_threads_left_idle() {
        let threads = Threads::new(2);
        let active = AtomicUsize::new(0);
        // Task 2 adds tasks 0 and 1, which wait for each other.
        let inner = |n: u32, added: &mut Vec<u32>| {
            if n == 2 {
                added.extend([0, 1]);
                return None;
            }
            active.fetch_add(1, Ordering::SeqCst);
            // Waits, for a long while at most, for the other task to start.
            let start = std::time::Instant::now();
            let overlapped = loop {
                if active.load(Ordering::SeqCst) == 2 {
                    break true;
                }
                if start.elapsed() > std::time::Duration::from_secs(10) {
                    break false;
                }
                core::hint::spin_loop();
            };
            Some(overlapped)
        };
        let outer = |_: u32, _: &mut Vec<u32>| {
            with_pool_in(&threads, &inner, |pool| pool.run(alloc::vec![2]))
        };
        let ran = with_pool_in(&threads, &outer, |pool| pool.run(alloc::vec![0]));
        let overlapped: Vec<_> = ran.into_iter().flatten().flatten().collect();
        assert_eq!(overlapped, [true, true]);
    }

    /// The two halves of a join run at once on a thread the budget spares,
    /// which they give back, and one after the other on one thread.
    #[cfg(feature = "std")]
    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
    fn join_runs_both_halves_at_once_on_a_spare_thread() {
        for limit in [1, 2] {
            let started = core::sync::atomic::AtomicBool::new(false);
            let threads = Threads::new(limit);
            let (overlapped, ran) = join(
                &threads,
                || {
                    if limit == 1 {
                        return started.load(Ordering::SeqCst);
                    }
                    // Waits, for a long while at most, for the other half.
                    let start = std::time::Instant::now();
                    while !started.load(Ordering::SeqCst) {
                        if start.elapsed() > std::time::Duration::from_secs(10) {
                            return false;
                        }
                        core::hint::spin_loop();
                    }
                    true
                },
                || {
                    started.store(true, Ordering::SeqCst);
                    threads.spare()
                },
            );
            assert_eq!(overlapped, limit == 2, "{limit} threads");
            assert_eq!(ran, 0, "the second half holds the spare thread");
            assert_eq!(threads.spare(), limit - 1);
        }
    }

    /// A task offered while the task that added it runs starts before that
    /// task returns, on a thread the budget spares; on one thread it runs
    /// after.
    #[cfg(feature = "std")]
    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
    fn offered_tasks_start_before_their_task_returns() {
        for limit in [1, 2] {
            let started = core::sync::atomic::AtomicBool::new(false);
            let work = |n: u32, added: &mut Added<'_, u32>| {
                if n == 1 {
                    started.store(true, Ordering::SeqCst);
                    return None;
                }
                added.push(1);
                added.offer();
                if limit == 1 {
                    return Some(started.load(Ordering::SeqCst));
                }
                // Waits, for a long while at most, for the offered task.
                let start = std::time::Instant::now();
                while !started.load(Ordering::SeqCst) {
                    if start.elapsed() > std::time::Duration::from_secs(10) {
                        return Some(false);
                    }
                    core::hint::spin_loop();
                }
                Some(true)
            };
            let threads = Threads::new(limit);
            let ran = with_offering_pool_in(&threads, &work, |pool| pool.run(alloc::vec![0]));
            let ran: Vec<bool> = ran.into_iter().flatten().collect();
            assert_eq!(ran, [limit == 2], "{limit} threads");
            assert!(started.load(Ordering::SeqCst));
        }
    }
}
