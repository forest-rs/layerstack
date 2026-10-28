// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The work pool itself: public only with the `unstable-internals` feature,
//! for this repository's format crates, and following no semver promise.
//! A host supplies threads through [`Threads`] and [`Executor`].

#[allow(
    clippy::wildcard_imports,
    reason = "the pool is the rest of its parent module"
)]
use super::*;

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
enum Tasks<'w, T, R> {
    Plain(&'w Work<'w, T, R>),
    Offering(&'w Offering<'w, T, R>),
}

impl<T, R> Clone for Tasks<'_, T, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T, R> Copy for Tasks<'_, T, R> {}

impl<T, R> Tasks<'_, T, R> {
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
    pool_in(threads, Tasks::Plain(work), body)
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
    pool_in(threads, Tasks::Offering(work), body)
}

/// Runs `a` on the calling thread and, at the same time, `b` on a thread
/// the budget `threads` spares, and returns both results. With no thread
/// to spare, or without the `std` feature, `b` runs after `a` on the
/// calling thread, as it does when the budget's [`Executor`] has not
/// started `b` by the time `a` returns. `b` holds its thread until it
/// returns, so work either runs on the same budget ([`with_pool_in`],
/// another `join`) takes the threads left.
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
        // Taken by whichever thread comes to it first: the job started for
        // it, or this one once `a` returns, so that `b` never waits on a job
        // that starts late, or not at all where threads cannot start (as on
        // `wasm32-wasip1`).
        let task = &std::sync::Mutex::new(Some(b));
        let helped = &std::sync::Mutex::new(None);
        let mut a = Some(a);
        let mut out = None;
        threads.executor().scope(&mut |scope| {
            scope.spawn(Box::new(move |_: &dyn Scope<'_>| {
                let taken = lock(task).take();
                if let Some(b) = taken {
                    // Resumed on the calling thread, as joining a thread
                    // resumes its panic.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(b));
                    *lock(helped) = Some(result);
                }
            }));
            let a = a.take().expect("a scope runs its body once")();
            let taken = lock(task).take();
            out = Some((a, taken.map(|b| b())));
        });
        threads.give_back();
        let (a, b) = out.expect("a scope runs its body");
        let b = b.unwrap_or_else(|| {
            let helped = lock(helped).take().expect("the second task runs once");
            helped.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        });
        return (a, b);
    }
    let _ = threads;
    (a(), b())
}

fn pool_in<T, R, B>(
    threads: &Threads,
    work: Tasks<'_, T, R>,
    body: impl FnOnce(&Pool<'_, T, R>) -> B,
) -> B
where
    T: Send,
    R: Send,
{
    #[cfg(feature = "std")]
    if threads.limit > 1 {
        let threaded = &Threaded {
            shared: std::sync::Mutex::new(Shared {
                pending: Vec::new(),
                busy: 0,
                workers: 0,
                cannot_start: false,
                results: Vec::new(),
                done: false,
            }),
            changed: std::sync::Condvar::new(),
            max_workers: threads.limit - 1,
            budget: threads,
            workers_wait: threads.executor.is_none(),
            waiting: AtomicUsize::new(0),
            callers_waiting: AtomicUsize::new(0),
        };
        let mut body = Some(body);
        let mut out = None;
        threads.executor().scope(&mut |scope| {
            let spawner = Spawner {
                scope,
                work,
                threaded,
            };
            let pool = Pool {
                work,
                threaded: Some((threaded, &spawner)),
            };
            let body = body.take().expect("a scope runs its body once");
            out = Some(body(&pool));
            lock(&threaded.shared).done = true;
            threaded.changed.notify_all();
        });
        return out.expect("a scope runs its body");
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
    work: Tasks<'s, T, R>,
    #[cfg(feature = "std")]
    threaded: Option<(&'s Threaded<'s, T, R>, &'s dyn Spawn)>,
}

/// Starts a worker for the pool.
#[cfg(feature = "std")]
trait Spawn {
    /// Starts a worker holding a thread claimed from the budget, as a job
    /// of the budget's executor; `false` where it cannot start one (as on
    /// `wasm32-wasip1`), leaving the tasks to the threads already on them.
    fn spawn_worker(&self) -> bool;
}

/// What starts a worker: copied into each worker, so that a task on any
/// thread can start one for the tasks it offers ([`Added::offer`]).
#[cfg(feature = "std")]
struct Spawner<'s, 'e, T, R> {
    scope: &'s dyn Scope<'e>,
    work: Tasks<'e, T, R>,
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
        let (work, threaded) = (self.work, self.threaded);
        self.scope.spawn(Box::new(move |scope: &dyn Scope<'_>| {
            serve(
                work,
                threaded,
                &Spawner {
                    scope,
                    work,
                    threaded,
                },
            );
        }))
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
    /// Whether a worker with no task waits for one until the pool shuts
    /// down, rather than return: on threads layerstack starts itself
    /// ([`OwnThreads`]), for which starting a worker each time tasks wait
    /// again costs more than waiting, and not on a host's executor, whose
    /// threads a waiting worker would park.
    workers_wait: bool,
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
    /// its pending tasks to the pool only while there is one, or while it
    /// can start a worker to take them.
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
    /// Workers started that have not returned.
    workers: usize,
    /// Whether the executor refused a worker: the pool starts no more.
    cannot_start: bool,
    results: Vec<R>,
    /// Whether the pool has shut down: its waiting workers return.
    done: bool,
}

#[cfg(feature = "std")]
impl<T, R> Shared<T, R> {
    /// Whether the pool may start another worker of the `most` it runs.
    fn can_start(&self, most: usize) -> bool {
        !self.cannot_start && self.workers < most
    }
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

/// Claims a thread from the budget for a worker to take tasks waiting
/// with no thread to take them, if the pool and the budget have room:
/// [`start_worker`] starts it once the lock is released.
#[cfg(feature = "std")]
fn reserve_worker<T, R>(threaded: &Threaded<'_, T, R>, shared: &mut Shared<T, R>) -> bool {
    if !shared.can_start(threaded.max_workers) || !threaded.budget.claim() {
        return false;
    }
    shared.workers += 1;
    true
}

/// Starts the worker [`reserve_worker`] claimed a thread for, without the
/// lock, so that it finds the tasks it is for; gives the thread back if
/// the executor cannot start it.
#[cfg(feature = "std")]
fn start_worker<T, R>(threaded: &Threaded<'_, T, R>, spawner: &dyn Spawn) {
    if spawner.spawn_worker() {
        return;
    }
    let mut shared = lock(&threaded.shared);
    shared.workers -= 1;
    shared.cannot_start = true;
    threaded.budget.give_back();
}

/// Takes a share of the pool's tasks, at least one, and does them and the
/// tasks they add without the lock, depth first, then records their
/// results. Pending tasks go back to the pool while a thread waits for
/// one, all but the next: the oldest, nearest the subtree's root, hold the
/// most work. When no thread waits, `spawner` starts a worker to take
/// them, if the pool and budget have room for one: on any thread when
/// workers return once idle, and on the pool's `caller` when they wait to
/// be handed tasks. Tasks offered from a task ([`Added::offer`]) may start
/// one on any thread.
#[cfg(feature = "std")]
fn work_on<'s, T, R>(
    work: Tasks<'_, T, R>,
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
    let mut can_spawn =
        (caller || !threaded.workers_wait) && shared.can_start(threaded.max_workers);
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
        // Tasks wait with no thread idle to take them: start one.
        let start = takers == 0 && can_spawn && reserve_worker(threaded, &mut shared);
        can_spawn &= shared.can_start(threaded.max_workers);
        // The pool's tasks are taken from its end: the oldest go there.
        shared.pending.extend(local.drain(..).rev());
        local.push(next);
        drop(shared);
        if start {
            start_worker(threaded, spawner);
        }
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
        let start = takers == 0 && reserve_worker(threaded, &mut shared);
        let handed = tasks.len();
        shared.pending.extend(tasks.drain(..).rev());
        drop(shared);
        if start {
            start_worker(threaded, self.spawner);
        }
        if handed >= threaded.takers() {
            threaded.changed.notify_all();
        } else {
            for _ in 0..handed {
                threaded.changed.notify_one();
            }
        }
    }
}

/// A worker's loop: does the pool's tasks, holding a thread of the budget
/// while it has tasks to do, until none waits. Then it gives the thread
/// back and returns, on a host's executor, so that it never parks one of
/// the host's threads, and the pool starts another when tasks wait again;
/// on threads of layerstack's own it waits for more until the pool shuts
/// down ([`Threaded::workers_wait`]).
#[cfg(feature = "std")]
fn serve<T, R>(work: Tasks<'_, T, R>, threaded: &Threaded<'_, T, R>, spawner: &dyn Spawn) {
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
        if shared.done || !threaded.workers_wait {
            shared.workers -= 1;
            return;
        }
        threaded.waiting.fetch_add(1, Ordering::Relaxed);
        shared = wait(threaded, shared);
        threaded.waiting.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
pub(super) mod tests {
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

    std::thread_local! {
        /// The threads started on this thread for budgets with no executor.
        pub(in crate::parallel) static OWN_STARTS: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
    }

    /// A host pool of two threads taking queued jobs first in, first out,
    /// whose threads waiting in a scope run that scope's jobs while they
    /// wait. A pool thread serves one scope's queue and returns when it is
    /// empty, so nested scopes share the two threads.
    #[cfg(feature = "std")]
    struct Fifo {
        /// The pool's threads serving no scope.
        free: AtomicUsize,
        /// The jobs started in every scope.
        jobs: AtomicUsize,
    }

    #[cfg(feature = "std")]
    impl Fifo {
        fn new() -> Self {
            Self {
                free: AtomicUsize::new(2),
                jobs: AtomicUsize::new(0),
            }
        }
    }

    /// One scope's jobs on a [`Fifo`], and how many of them run.
    #[cfg(feature = "std")]
    struct FifoQueue<'env> {
        state: std::sync::Mutex<FifoState<'env>>,
        changed: std::sync::Condvar,
    }

    #[cfg(feature = "std")]
    struct FifoState<'env> {
        jobs: std::collections::VecDeque<Job<'env>>,
        running: usize,
    }

    #[cfg(feature = "std")]
    struct FifoScope<'s, 'e, 'env> {
        pool: &'e Fifo,
        queue: &'e FifoQueue<'env>,
        threads: &'s std::thread::Scope<'s, 'e>,
    }

    #[cfg(feature = "std")]
    impl FifoScope<'_, '_, '_> {
        /// Runs queued jobs until none is left; `false` once the queue is
        /// empty, `true` after a job.
        fn run_one(&self) -> bool {
            let mut state = lock(&self.queue.state);
            let Some(job) = state.jobs.pop_front() else {
                return false;
            };
            state.running += 1;
            drop(state);
            job(self);
            lock(&self.queue.state).running -= 1;
            self.queue.changed.notify_all();
            true
        }
    }

    #[cfg(feature = "std")]
    impl<'env> Scope<'env> for FifoScope<'_, '_, 'env> {
        fn spawn(&self, job: Job<'env>) -> bool {
            self.pool.jobs.fetch_add(1, Ordering::SeqCst);
            lock(&self.queue.state).jobs.push_back(job);
            self.queue.changed.notify_all();
            let free = &self.pool.free;
            let mut now = free.load(Ordering::SeqCst);
            let claimed = loop {
                if now == 0 {
                    break false;
                }
                match free.compare_exchange(now, now - 1, Ordering::SeqCst, Ordering::SeqCst) {
                    Ok(_) => break true,
                    Err(seen) => now = seen,
                }
            };
            if claimed {
                let (pool, queue, threads) = (self.pool, self.queue, self.threads);
                threads.spawn(move || {
                    let scope = FifoScope {
                        pool,
                        queue,
                        threads,
                    };
                    while scope.run_one() {}
                    pool.free.fetch_add(1, Ordering::SeqCst);
                });
            }
            true
        }
    }

    #[cfg(feature = "std")]
    impl Executor for Fifo {
        fn scope<'env>(&self, body: &mut (dyn FnMut(&dyn Scope<'env>) + 'env)) {
            let queue = FifoQueue {
                state: std::sync::Mutex::new(FifoState {
                    jobs: std::collections::VecDeque::new(),
                    running: 0,
                }),
                changed: std::sync::Condvar::new(),
            };
            std::thread::scope(|threads| {
                let scope = FifoScope {
                    pool: self,
                    queue: &queue,
                    threads,
                };
                body(&scope);
                // Helps while it waits: runs queued jobs on this thread.
                loop {
                    if scope.run_one() {
                        continue;
                    }
                    let state = lock(&queue.state);
                    if state.jobs.is_empty() && state.running == 0 {
                        break;
                    }
                    if state.jobs.is_empty() {
                        drop(queue.changed.wait(state));
                    }
                }
            });
        }
    }

    /// A host executor that never runs a job at the same time as anything
    /// else: it queues them, and runs them on the waiting thread once the
    /// body returns, first in, first out.
    #[cfg(feature = "std")]
    struct Deferred {
        /// The jobs started in every scope.
        jobs: AtomicUsize,
    }

    #[cfg(feature = "std")]
    struct DeferredScope<'a, 'env> {
        jobs: std::sync::Mutex<std::collections::VecDeque<Job<'env>>>,
        started: &'a AtomicUsize,
    }

    #[cfg(feature = "std")]
    impl<'env> Scope<'env> for DeferredScope<'_, 'env> {
        fn spawn(&self, job: Job<'env>) -> bool {
            self.started.fetch_add(1, Ordering::SeqCst);
            lock(&self.jobs).push_back(job);
            true
        }
    }

    #[cfg(feature = "std")]
    impl Executor for Deferred {
        fn scope<'env>(&self, body: &mut (dyn FnMut(&dyn Scope<'env>) + 'env)) {
            let scope = DeferredScope {
                jobs: std::sync::Mutex::new(std::collections::VecDeque::new()),
                started: &self.jobs,
            };
            body(&scope);
            loop {
                let job = lock(&scope.jobs).pop_front();
                match job {
                    Some(job) => job(&scope),
                    None => break,
                }
            }
        }
    }

    /// The host executors under test, each on a budget of `limit` threads,
    /// with the count of jobs it has started.
    #[cfg(feature = "std")]
    fn host_budgets(limit: usize) -> [(Threads, Arc<dyn Fn() -> usize + Send + Sync>); 2] {
        let fifo = Arc::new(Fifo::new());
        let deferred = Arc::new(Deferred {
            jobs: AtomicUsize::new(0),
        });
        let fifo_jobs = Arc::clone(&fifo);
        let deferred_jobs = Arc::clone(&deferred);
        [
            (
                Threads::with_executor(limit, fifo),
                Arc::new(move || fifo_jobs.jobs.load(Ordering::SeqCst)),
            ),
            (
                Threads::with_executor(limit, deferred),
                Arc::new(move || deferred_jobs.jobs.load(Ordering::SeqCst)),
            ),
        ]
    }

    /// The results of every kind of work on `threads`, sorted where their
    /// order is not fixed: two phases of a tree, an offering tree, pools
    /// nested in a pool's tasks, and joins nested in a join.
    #[cfg(feature = "std")]
    fn every_kind_of_work(threads: &Threads) -> Vec<Vec<u64>> {
        let mut plain = with_pool_in(threads, &tree, |pool| {
            let mut first = pool.run(alloc::vec![10, 3]);
            first.extend(pool.run(alloc::vec![4]));
            first
        });
        plain.sort_unstable();
        let offering = |n: u32, added: &mut Added<'_, u32>| {
            if n > 0 {
                added.push(n - 1);
                added.push(n - 1);
                added.offer();
            }
            n
        };
        let mut offered =
            with_offering_pool_in(threads, &offering, |pool| pool.run(alloc::vec![9]));
        offered.sort_unstable();
        let inner = |n: u64, _: &mut Vec<u64>| n;
        let outer = |n: u64, added: &mut Vec<u64>| -> u64 {
            if n > 10 {
                added.push(n - 10);
            }
            with_pool_in(threads, &inner, |pool| pool.run((0..n).collect()))
                .into_iter()
                .sum()
        };
        let mut nested = with_pool_in(threads, &outer, |pool| pool.run(alloc::vec![100, 75]));
        nested.sort_unstable();
        let (left, (middle, right)) = join(
            threads,
            || (0..1000_u64).sum::<u64>(),
            || join(threads, || (0..10_u64).product::<u64>(), || 7_u64),
        );
        assert_eq!(
            threads.spare(),
            threads.limit() - 1,
            "every thread given back"
        );
        alloc::vec![
            plain.into_iter().map(u64::from).collect(),
            offered.into_iter().map(u64::from).collect(),
            nested,
            alloc::vec![left, middle, right],
        ]
    }

    /// Host executors, one running jobs on two threads of its own and one
    /// running them only once the body returns, give every kind of work
    /// the results threads of layerstack's own do, and run the work's
    /// helpers as their jobs.
    #[cfg(feature = "std")]
    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
    fn host_executors_do_the_work_layerstack_threads_do() {
        let expected = every_kind_of_work(&Threads::new(1));
        assert_eq!(every_kind_of_work(&Threads::new(4)), expected);
        for limit in [2, 8] {
            for (threads, jobs) in host_budgets(limit) {
                for _ in 0..3 {
                    assert_eq!(every_kind_of_work(&threads), expected, "{threads:?}");
                }
                assert!(jobs() > 0, "{threads:?} started no job");
            }
        }
    }

    /// Pools nested in a pool's tasks on a host executor never run more
    /// threads together than the budget holds.
    #[cfg(feature = "std")]
    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
    fn nested_pools_on_a_host_executor_keep_to_the_budget() {
        for (threads, _) in host_budgets(3) {
            let running = Running {
                now: AtomicUsize::new(0),
                most: AtomicUsize::new(0),
            };
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
    }

    /// A stage of groups of prims, some instancing an asset in a second
    /// layer: wide enough for every thread to take work.
    #[cfg(feature = "std")]
    fn wide_stage() -> crate::InMemoryStore {
        use crate::{InMemoryStore, Layer, LayerId, PrimSpec, Reference, Value};

        let mut store = InMemoryStore::default();
        let field = store.tokens.intern("value");
        let asset = store.path("/Asset");
        let part = store.tokens.intern("Part");
        let mut source = Layer::new(LayerId(2));
        source.insert_prim(asset, PrimSpec::def().with_children(alloc::vec![part]));
        let part_path = store.path("/Asset/Part");
        source.insert_prim(part_path, PrimSpec::def().with_field(field, Value::Int(1)));
        store.insert_layer(source);
        let mut root = Layer::new(LayerId(1));
        for group in 0..16 {
            let names = (0..24)
                .map(|item| store.tokens.intern(alloc::format!("Item{item}")))
                .collect();
            let group_path = store.path(&alloc::format!("/Group{group}"));
            root.insert_prim(group_path, PrimSpec::def().with_children(names));
            for item in 0..24 {
                let path = store.path(&alloc::format!("/Group{group}/Item{item}"));
                let spec = PrimSpec::def()
                    .with_field(field, Value::Int(item))
                    .with_instanceable(item % 3 == 0)
                    .with_reference(Reference::with_asset(LayerId(2), asset, "asset.usda"));
                root.insert_prim(path, spec);
            }
        }
        store.insert_layer(root);
        store
    }

    /// Every prim of `wide_stage` composed with `options`, with its value.
    #[cfg(feature = "std")]
    fn render_wide_stage(options: crate::StageOptions) -> Vec<alloc::string::String> {
        use crate::{LayerId, Path, Stage};

        let mut store = wide_stage();
        let field = store.tokens.intern("value");
        let stage = Stage::compose(&mut store, LayerId(1), options);
        let root = store.paths.lookup(&Path::root()).expect("the root path");
        let mut prims: Vec<_> = stage
            .traverse_all(root)
            .map(|prim| {
                let value = stage.resolve_field(prim, field).map(|value| value.value);
                alloc::format!("{} {value:?}", store.paths.display(prim, &store.tokens))
            })
            .collect();
        prims.sort_unstable();
        prims
    }

    /// A stage composed on host executors is the stage one thread composes.
    #[cfg(feature = "std")]
    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
    fn a_stage_composes_alike_on_host_executors() {
        let single = render_wide_stage(crate::StageOptions::default());
        assert!(single.len() > 16 * 24 * 2, "{} prims", single.len());
        for (threads, jobs) in host_budgets(4) {
            let executor = threads.executor.clone();
            let options = crate::StageOptions {
                threads: 4,
                executor,
                with_dependencies: true,
                ..crate::StageOptions::default()
            };
            assert_eq!(render_wide_stage(options), single, "{threads:?}");
            assert!(jobs() > 0, "{threads:?} started no job");
        }
    }

    /// Composing on threads with an executor starts every helper as its
    /// job, and no thread of layerstack's own, as composing without one
    /// does. The executor runs jobs on the composing thread, so a thread
    /// started anywhere for the stage would be counted on it.
    #[cfg(feature = "std")]
    #[test]
    fn every_budget_a_stage_builds_runs_on_its_executor() {
        let deferred = Arc::new(Deferred {
            jobs: AtomicUsize::new(0),
        });
        let options = crate::StageOptions {
            threads: 4,
            executor: Some(Arc::clone(&deferred) as Arc<dyn Executor>),
            with_dependencies: true,
            ..crate::StageOptions::default()
        };
        let before = OWN_STARTS.with(core::cell::Cell::get);
        let mut store = wide_stage();
        let stage = crate::Stage::compose(&mut store, crate::LayerId(1), options.clone());
        let live = crate::LiveStage::compose(&mut store, crate::LayerId(1), options);
        assert!(stage.composition_errors().is_empty());
        drop(live);
        assert!(deferred.jobs.load(Ordering::SeqCst) > 0);
        assert_eq!(OWN_STARTS.with(core::cell::Cell::get), before);

        let own = crate::StageOptions {
            threads: 4,
            with_dependencies: true,
            ..crate::StageOptions::default()
        };
        drop(crate::Stage::compose(&mut store, crate::LayerId(1), own));
        assert!(OWN_STARTS.with(core::cell::Cell::get) > before);
    }
}
