//! Deadlock detection for blocking mutexes and semaphores.
//!
//! Deadlock detection is enabled per process by `sys_enable_deadlock_detect`.
//! When the current task is about to block on a blocking mutex or a
//! semaphore, the kernel first checks whether the current task can ever make
//! progress after blocking. If it cannot, the waiting syscall returns
//! `-0xdead` to user space instead of blocking, so that user programs can
//! observe and recover from the deadlock.
//!
//! A task can make progress if:
//! - it is not blocked (running or ready to run), or
//! - it is blocked on something else than a mutex/semaphore (e.g. a timer),
//! - it waits for a mutex and the owner of the mutex can make progress, or
//! - it waits for a semaphore and at least one holder of the semaphore can
//!   make progress (a semaphore with no holder is considered as progressable
//!   because someone else may still `up` it).

use crate::task::{ProcessControlBlockInner, TaskStatus};
use alloc::collections::BTreeSet;

/// The resource that a blocked task is waiting for.
///
/// It is recorded in the task control block right before the task blocks,
/// and cleared when the task is woken up. Tasks blocked on other resources
/// (timer, condvar, ...) have no wait target.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum WaitTarget {
    /// waiting for a blocking mutex
    Mutex(usize),
    /// waiting for a semaphore
    Semaphore(usize),
}

/// Check whether the current task will deadlock if it blocks on `target`.
///
/// `inner` is the inner of the process the current task belongs to, `tid` is
/// the tid of the current task.
pub fn detect_deadlock(
    inner: &ProcessControlBlockInner,
    current_tid: usize,
    target: WaitTarget,
) -> bool {
    let mut visiting = BTreeSet::new();
    !target_can_progress(inner, target, current_tid, &mut visiting)
}

/// Whether the resource described by `target` will eventually be released.
fn target_can_progress(
    inner: &ProcessControlBlockInner,
    target: WaitTarget,
    current_tid: usize,
    visiting: &mut BTreeSet<usize>,
) -> bool {
    match target {
        WaitTarget::Mutex(mutex_id) => {
            let owner = inner
                .mutex_list
                .get(mutex_id)
                .and_then(|mutex| mutex.as_ref())
                .and_then(|mutex| mutex.owner());
            match owner {
                Some(owner) => task_can_progress(inner, owner, current_tid, visiting),
                None => true,
            }
        }
        WaitTarget::Semaphore(sem_id) => {
            match inner.semaphore_list.get(sem_id).and_then(|sem| sem.as_ref()) {
                Some(sem) => {
                    if sem.count() > 0 {
                        // a free resource exists
                        return true;
                    }
                    let holders = sem.holders();
                    if holders.is_empty() {
                        // nobody has taken a resource, someone may still up() it
                        return true;
                    }
                    holders
                        .iter()
                        .any(|holder| task_can_progress(inner, *holder, current_tid, visiting))
                }
                None => true,
            }
        }
    }
}

/// Whether a task can eventually make progress (and release what it holds).
fn task_can_progress(
    inner: &ProcessControlBlockInner,
    tid: usize,
    current_tid: usize,
    visiting: &mut BTreeSet<usize>,
) -> bool {
    if tid == current_tid {
        // the current task is the one that is about to block
        return false;
    }
    let task = match inner.tasks.get(tid).and_then(|task| task.as_ref()) {
        Some(task) => task,
        // the task has already exited (or never existed)
        None => return true,
    };
    let (status, wait_target) = {
        let task_inner = task.inner_exclusive_access();
        (task_inner.task_status, task_inner.wait_target)
    };
    if status != TaskStatus::Blocked {
        return true;
    }
    let wait_target = match wait_target {
        Some(wait_target) => wait_target,
        // blocked on a timer/condvar/...: it will be woken up eventually
        None => return true,
    };
    // A cycle without any runnable task in it can never make progress.
    if !visiting.insert(tid) {
        return false;
    }
    let can_progress = target_can_progress(inner, wait_target, current_tid, visiting);
    visiting.remove(&tid);
    can_progress
}
