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
//! budget's limit; a phase with one task at a time starts none. A worker
//! with no task gives its thread back to the budget until it has one
//! again, so a task that runs a pool of its own on the same budget, as
//! reading one layer of a stage opened on threads does, can use the
//! threads the outer pool is not using, and the two never run more threads
//! than the budget holds.
//!
//! Workers run on the budget's [`Executor`]: threads layerstack starts for
//! the pool by default, which wait for tasks as long as [`with_pool`]
//! runs, or a host's job system, thread pool or dispatch queue
//! ([`Threads::with_executor`]), on which a worker returns once no task
//! waits, so that it never parks a thread of the host's, and the pool
//! starts another when tasks wait again.
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

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::fmt;
#[cfg(feature = "std")]
use core::sync::atomic::{AtomicUsize, Ordering};

/// Where layerstack runs its work beyond the calling thread: a host's job
/// system, thread pool or dispatch queue, in place of the threads
/// layerstack starts itself by default.
///
/// The trait, [`Scope`] and [`Job`] exist in every build, so a host names
/// them, and sets [`crate::StageOptions::executor`], whatever features the
/// crate graph enables. Without the `std` feature work runs on the calling
/// thread only, and an executor is held but never called, as
/// [`crate::StageOptions::threads`] is then ignored.
///
/// A budget runs on one ([`Threads::with_executor`]), as composition and
/// `layerstack_io::StageDocument`'s reading do with
/// [`crate::StageOptions::executor`]. Each pool and each [`join`] on the
/// budget calls [`Self::scope`] once and starts jobs on its scope: at most
/// as many at once, across every pool on the budget, as the budget's
/// [`Threads::limit`] spares beyond the calling thread. The executor's own
/// size bounds how many threads those jobs occupy; layerstack starts no
/// thread of its own when a budget has an executor.
///
/// # Contract
///
/// Every job layerstack starts is a helper. The thread that calls `scope`
/// does all of the work by itself when no job runs, so a job that starts
/// late, or only after the body returns, costs parallelism, never progress;
/// a job that finds nothing left to do returns at once. A job holds a
/// thread only while it has work, so jobs do not park the executor's
/// threads. A job that runs a pool of its own waits for that pool's tasks
/// other threads are doing, but never for a job that has not started: no
/// thread waits on work no thread is doing. A job may start more
/// jobs on the scope it is given, from any thread, and may itself call
/// `scope` on the same executor, when a task runs a pool of its own; any
/// number of threads may be in `scope` at once, as when several stages
/// compose on one executor.
///
/// An executor's `scope` must:
///
/// - call `body` exactly once, on the calling thread;
/// - run every job [`Scope::spawn`] accepts exactly once, on any thread,
///   passing it a scope that starts jobs in the same scope;
/// - return only once `body` and every job it accepted have returned,
///   including the jobs those jobs start;
/// - while it waits for jobs, run the jobs not yet started on the waiting
///   thread itself, as `rayon::scope` does, and as `dispatch_apply` runs
///   the iterations no thread has taken on its caller, so that a scope
///   called inside a job never waits on a thread that is not coming.
///
/// An executor whose waiting threads block without running jobs may
/// deadlock: once every one of its threads waits in a scope, the jobs those
/// scopes wait for have no thread to run on.
///
/// If `body` or a job panics, `scope` should still wait for every job, then
/// resume the panic, as `std::thread::scope` does. Executors that cannot
/// start a job, as where threads cannot start (`wasm32-wasip1`), refuse it
/// ([`Scope::spawn`] returns `false`) and the calling thread does its work.
///
/// Implementing it needs no `unsafe` when the host's pool has a scope of
/// its own: over rayon, `scope` is `rayon::in_place_scope(|s| body(&H(s)))`
/// for a wrapper `H(&rayon::Scope<'env>)`, whose `spawn` is
/// `self.0.spawn(move |s| job(&H(s)))` returning `true`; rayon's scopes run
/// waiting jobs on the waiting thread. A host queue that takes only
/// `'static` jobs cannot hold these without erasing `'env`, which takes
/// `unsafe` in the host and is sound only because `scope` returns after
/// every job it accepted; a pool's own scoped API, where it has one, needs
/// neither.
///
/// OpenUSD runs its parallel work on one shared pool: each `WorkDispatcher`
/// runs tasks on the process's TBB-backed thread pool, sized by
/// `WorkSetConcurrencyLimit`, and `WorkDispatcher::Wait` runs waiting tasks
/// on the thread that waits (`pxr/base/work/dispatcher.h`,
/// `pxr/base/work/threadLimits.h`).
///
/// ```
/// use std::sync::{Arc, Mutex};
///
/// use layerstack::parallel::{Executor, Job, Scope, Threads, with_pool_in};
///
/// /// A host executor that runs every job on the thread that waits for it,
/// /// once the body returns: a serial queue. A threaded one would also hand
/// /// queued jobs to its own threads.
/// struct Serial;
///
/// /// The jobs a scope has queued.
/// struct Queue<'env> {
///     jobs: Mutex<Vec<Job<'env>>>,
/// }
///
/// impl<'env> Scope<'env> for Queue<'env> {
///     fn spawn(&self, job: Job<'env>) -> bool {
///         self.jobs.lock().unwrap().push(job);
///         true
///     }
/// }
///
/// impl Executor for Serial {
///     fn scope<'env>(&self, body: &mut (dyn FnMut(&dyn Scope<'env>) + 'env)) {
///         let queue = Queue {
///             jobs: Mutex::new(Vec::new()),
///         };
///         body(&queue);
///         // Help while waiting: run the jobs left, and the jobs they start.
///         loop {
///             let job = queue.jobs.lock().unwrap().pop();
///             match job {
///                 Some(job) => job(&queue),
///                 None => break,
///             }
///         }
///     }
/// }
///
/// // Each task `n` adds `n - 1` twice, down to 0.
/// let tree = |n: u32, added: &mut Vec<u32>| {
///     if n > 0 {
///         added.extend([n - 1, n - 1]);
///     }
///     n
/// };
/// let threads = Threads::with_executor(4, Arc::new(Serial));
/// let results = with_pool_in(&threads, &tree, |pool| pool.run(vec![3]));
/// assert_eq!(results.len(), 15);
/// ```
pub trait Executor: Send + Sync {
    /// Calls `body` on the calling thread with a scope it may start jobs on,
    /// and returns once `body` and every job started on the scope have
    /// returned, running jobs not yet started on this thread while it waits
    /// (see [the contract](Executor#contract)).
    fn scope<'env>(&self, body: &mut (dyn FnMut(&dyn Scope<'env>) + 'env));
}

impl fmt::Debug for dyn Executor + '_ {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Executor").finish_non_exhaustive()
    }
}

/// A job layerstack starts on an [`Executor`]'s [`Scope`]: a helper that
/// may borrow from the caller of [`Executor::scope`] (`'env`) and is given
/// the scope it runs in, so that it can start more jobs there.
pub type Job<'env> = Box<dyn FnOnce(&dyn Scope<'env>) + Send + 'env>;

/// What an [`Executor`] gives the body of [`Executor::scope`] and each job
/// started in it: starts jobs that may borrow from `'env`, all of which
/// return before `scope` does.
pub trait Scope<'env>: Sync {
    /// Starts `job`, on any thread of the executor and at any time before
    /// the scope returns, and returns `true`; or returns `false`, dropping
    /// `job` unrun, if it cannot start one, as where threads cannot start
    /// (`wasm32-wasip1`), and the calling thread does the work instead.
    /// It may run `job` on the calling thread before returning, though that
    /// only delays the work that started it.
    fn spawn(&self, job: Job<'env>) -> bool;
}

/// The executor of a budget with none given: a thread started for each job
/// (`std::thread::Builder::spawn_scoped`), all of them joined when the scope
/// ends (`std::thread::scope`).
#[cfg(feature = "std")]
struct OwnThreads;

#[cfg(feature = "std")]
impl Executor for OwnThreads {
    fn scope<'env>(&self, body: &mut (dyn FnMut(&dyn Scope<'env>) + 'env)) {
        std::thread::scope(|scope| body(&OwnScope { scope }));
    }
}

/// The scope of [`OwnThreads`].
#[cfg(feature = "std")]
struct OwnScope<'s, 'env> {
    scope: &'s std::thread::Scope<'s, 'env>,
}

#[cfg(feature = "std")]
impl<'env> Scope<'env> for OwnScope<'_, 'env> {
    fn spawn(&self, job: Job<'env>) -> bool {
        #[cfg(test)]
        pool::tests::OWN_STARTS.with(|starts| starts.set(starts.get() + 1));
        let scope = self.scope;
        std::thread::Builder::new()
            .spawn_scoped(scope, move || job(&OwnScope { scope }))
            .is_ok()
    }
}

/// The threads that work may run on at once, the calling thread among
/// them: a budget that every pool started under it shares ([`with_pool_in`]).
///
/// A pool starts a worker only when the budget has a thread to spare, and
/// a worker gives its thread back while no task waits for it. So a pool run
/// from a task of another pool on the same budget runs on the threads the
/// other is not using at the time, and the two together never run more
/// than [`Self::limit`] threads. Without the `std` feature every pool runs
/// on the calling thread.
///
/// Workers run on the budget's [`Executor`]: threads layerstack starts by
/// default ([`Self::new`]), or a host's ([`Self::with_executor`]).
pub struct Threads {
    limit: usize,
    /// The threads beyond the calling thread that no worker holds.
    #[cfg(feature = "std")]
    spare: AtomicUsize,
    /// The host's executor; `None` starts threads of layerstack's own.
    /// Held but unused without `std`.
    executor: Option<Arc<dyn Executor>>,
}

impl Threads {
    /// A budget of `limit` threads, the calling thread among them; `0` and
    /// `1` run every pool on the calling thread. Workers run on threads
    /// layerstack starts for each pool and [`join`], and joins when they
    /// end (`std::thread::scope`).
    pub const fn new(limit: usize) -> Self {
        let limit = if limit == 0 { 1 } else { limit };
        Self {
            limit,
            #[cfg(feature = "std")]
            spare: AtomicUsize::new(limit - 1),
            executor: None,
        }
    }

    /// A budget of `limit` threads, the calling thread among them, whose
    /// workers run as jobs of `executor`: at most `limit - 1` jobs at once,
    /// on as many threads as the executor gives them. `0` and `1` run every
    /// pool on the calling thread and start no job. Without the `std`
    /// feature every pool runs on the calling thread, and `executor` is
    /// never called.
    pub fn with_executor(limit: usize, executor: Arc<dyn Executor>) -> Self {
        Self {
            executor: Some(executor),
            ..Self::new(limit)
        }
    }

    /// The executor this budget's workers run on.
    #[cfg(feature = "std")]
    fn executor(&self) -> &dyn Executor {
        match &self.executor {
            Some(executor) => &**executor,
            None => &OwnThreads,
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
        let mut debug = f.debug_struct("Threads");
        debug
            .field("limit", &self.limit)
            .field("spare", &self.spare());
        debug.field("executor", &self.executor);
        debug.finish()
    }
}

#[cfg_attr(
    not(feature = "unstable-internals"),
    allow(
        unreachable_pub,
        dead_code,
        reason = "public only with the `unstable-internals` feature"
    )
)]
mod pool;
#[cfg(feature = "unstable-internals")]
pub use pool::{Added, Offering, Pool, Work, join, with_offering_pool_in, with_pool, with_pool_in};
#[cfg(not(feature = "unstable-internals"))]
#[allow(unused_imports, reason = "not every pool item is used in this crate")]
pub(crate) use pool::{
    Added, Offering, Pool, Work, join, with_offering_pool_in, with_pool, with_pool_in,
};
