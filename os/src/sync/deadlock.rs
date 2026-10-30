//! 死锁检测用的等待图

use super::UPSafeCell;
use crate::task::TaskControlBlock;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::*;

lazy_static! {
    // 任务地址 -> 它正在等的任务们
    static ref WAIT_FOR: UPSafeCell<BTreeMap<usize, Vec<usize>>> =
        unsafe { UPSafeCell::new(BTreeMap::new()) };
}

/// 取任务在等待图里的编号，直接用 TCB 的地址
pub fn task_key(task: &Arc<TaskControlBlock>) -> usize {
    Arc::as_ptr(task) as usize
}

/// 把「当前任务等 blocking_on」这条边算进去后，等待图里会不会成环
pub fn would_deadlock(waiter: usize, blocking_on: &[usize]) -> bool {
    if blocking_on.contains(&waiter) {
        // 自己等自己，比如对同一个互斥锁连锁两次
        return true;
    }
    let graph = WAIT_FOR.exclusive_access();
    let mut stack = blocking_on.to_vec();
    let mut visited = Vec::new();
    while let Some(node) = stack.pop() {
        if node == waiter {
            return true;
        }
        if visited.contains(&node) {
            continue;
        }
        visited.push(node);
        if let Some(next) = graph.get(&node) {
            stack.extend(next.iter().copied());
        }
    }
    false
}

/// 记下当前任务在等哪些任务
pub fn push_wait(waiter: usize, blocking_on: &[usize]) {
    WAIT_FOR
        .exclusive_access()
        .insert(waiter, blocking_on.to_vec());
}

/// 拿到资源或者退出时，把任务从等待图里删掉
pub fn remove_wait(waiter: usize) {
    WAIT_FOR.exclusive_access().remove(&waiter);
}
