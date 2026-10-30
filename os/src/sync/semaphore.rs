//! Semaphore

use crate::sync::deadlock::{push_wait, remove_wait, task_key, would_deadlock};
use crate::sync::{deadlock_detect_enabled, UPSafeCell};
use crate::task::{block_current_and_run_next, current_task, wakeup_task, TaskControlBlock};
use alloc::{collections::VecDeque, sync::Arc, vec::Vec};

/// semaphore structure
pub struct Semaphore {
    /// semaphore inner
    pub inner: UPSafeCell<SemaphoreInner>,
}

pub struct SemaphoreInner {
    pub count: isize,
    pub wait_queue: VecDeque<Arc<TaskControlBlock>>,
    /// 占着资源实例的任务，死锁检测用
    pub holders: Vec<Arc<TaskControlBlock>>,
}

impl Semaphore {
    /// Create a new semaphore
    pub fn new(res_count: usize) -> Self {
        trace!("kernel: Semaphore::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(SemaphoreInner {
                    count: res_count as isize,
                    wait_queue: VecDeque::new(),
                    holders: Vec::new(),
                })
            },
        }
    }

    /// up operation of semaphore
    pub fn up(&self) {
        trace!("kernel: Semaphore::up");
        let task = current_task().unwrap();
        let mut inner = self.inner.exclusive_access();
        inner.count += 1;
        // 释放自己占的一份
        let key = task_key(&task);
        if let Some(pos) = inner.holders.iter().position(|h| task_key(h) == key) {
            inner.holders.remove(pos);
        }
        if inner.count <= 0 {
            if let Some(task) = inner.wait_queue.pop_front() {
                if deadlock_detect_enabled() {
                    remove_wait(task_key(&task));
                }
                inner.holders.push(task.clone());
                wakeup_task(task);
            }
        }
    }

    /// down operation of semaphore, return false if it would deadlock
    pub fn down(&self) -> bool {
        trace!("kernel: Semaphore::down");
        let task = current_task().unwrap();
        let mut inner = self.inner.exclusive_access();
        inner.count -= 1;
        if inner.count < 0 {
            if deadlock_detect_enabled() {
                let waiter = task_key(&task);
                let blockers: Vec<usize> = inner
                    .holders
                    .iter()
                    .map(task_key)
                    .filter(|key| *key != waiter)
                    .collect();
                let dead = would_deadlock(waiter, &blockers);
                if dead {
                    warn!("kernel: deadlock detected when down a semaphore");
                    inner.count += 1;
                    return false;
                }
                push_wait(waiter, &blockers);
            }
            inner.wait_queue.push_back(task);
            drop(inner);
            block_current_and_run_next();
            // 醒来时 up 已经把这一份记到我名下了，这里不要再记一遍
        } else {
            inner.holders.push(task);
        }
        true
    }
}
