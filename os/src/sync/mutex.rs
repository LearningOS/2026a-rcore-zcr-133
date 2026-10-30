//! Mutex (spin-like and blocking(sleep))

use super::deadlock::{push_wait, remove_wait, task_key, would_deadlock};
use super::{deadlock_detect_enabled, UPSafeCell};
use crate::task::TaskControlBlock;
use crate::task::{block_current_and_run_next, suspend_current_and_run_next};
use crate::task::{current_task, wakeup_task};
use alloc::{collections::VecDeque, sync::Arc, vec::Vec};

/// Mutex trait
pub trait Mutex: Sync + Send {
    /// Lock the mutex, return false if the lock would deadlock
    fn lock(&self) -> bool;
    /// Unlock the mutex
    fn unlock(&self);
}

/// Spinlock Mutex struct
pub struct MutexSpin {
    locked: UPSafeCell<bool>,
}

impl MutexSpin {
    /// Create a new spinlock mutex
    pub fn new() -> Self {
        Self {
            locked: unsafe { UPSafeCell::new(false) },
        }
    }
}

impl Mutex for MutexSpin {
    /// Lock the spinlock mutex
    fn lock(&self) -> bool {
        trace!("kernel: MutexSpin::lock");
        loop {
            let mut locked = self.locked.exclusive_access();
            if *locked {
                drop(locked);
                suspend_current_and_run_next();
                continue;
            } else {
                *locked = true;
                return true;
            }
        }
    }

    fn unlock(&self) {
        trace!("kernel: MutexSpin::unlock");
        let mut locked = self.locked.exclusive_access();
        *locked = false;
    }
}

/// Blocking Mutex struct
pub struct MutexBlocking {
    inner: UPSafeCell<MutexBlockingInner>,
}

pub struct MutexBlockingInner {
    locked: bool,
    wait_queue: VecDeque<Arc<TaskControlBlock>>,
    /// 持锁的任务，死锁检测用
    holder: Option<Arc<TaskControlBlock>>,
}

impl MutexBlocking {
    /// Create a new blocking mutex
    pub fn new() -> Self {
        trace!("kernel: MutexBlocking::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(MutexBlockingInner {
                    locked: false,
                    wait_queue: VecDeque::new(),
                    holder: None,
                })
            },
        }
    }
}

impl Mutex for MutexBlocking {
    /// lock the blocking mutex
    fn lock(&self) -> bool {
        trace!("kernel: MutexBlocking::lock");
        let mut mutex_inner = self.inner.exclusive_access();
        if mutex_inner.locked {
            let task = current_task().unwrap();
            if deadlock_detect_enabled() {
                let waiter = task_key(&task);
                let blockers: Vec<usize> = mutex_inner
                    .holder
                    .iter()
                    .map(task_key)
                    .collect();
                if would_deadlock(waiter, &blockers) {
                    warn!("kernel: deadlock detected when locking a mutex");
                    return false;
                }
                push_wait(waiter, &blockers);
            }
            mutex_inner.wait_queue.push_back(task);
            drop(mutex_inner);
            block_current_and_run_next();
        } else {
            mutex_inner.locked = true;
            mutex_inner.holder = Some(current_task().unwrap());
        }
        true
    }

    /// unlock the blocking mutex
    fn unlock(&self) {
        trace!("kernel: MutexBlocking::unlock");
        let mut mutex_inner = self.inner.exclusive_access();
        assert!(mutex_inner.locked);
        mutex_inner.holder = None;
        if let Some(waking_task) = mutex_inner.wait_queue.pop_front() {
            if deadlock_detect_enabled() {
                remove_wait(task_key(&waking_task));
            }
            // 锁直接转给被唤醒的任务
            mutex_inner.holder = Some(waking_task.clone());
            wakeup_task(waking_task);
        } else {
            mutex_inner.locked = false;
        }
    }
}
