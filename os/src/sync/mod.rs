//! Synchronization and interior mutability primitives

mod condvar;
mod deadlock;
mod mutex;
mod semaphore;
mod up;

pub use condvar::Condvar;
pub use deadlock::{push_wait, remove_wait, task_key, would_deadlock};
pub use mutex::{Mutex, MutexBlocking, MutexSpin};
pub use semaphore::Semaphore;
pub use up::UPSafeCell;

use core::sync::atomic::{AtomicBool, Ordering};

static DEADLOCK_DETECT_ENABLED: AtomicBool = AtomicBool::new(false);

/// 打开或关闭死锁检测
pub fn set_deadlock_detect(enabled: bool) {
    DEADLOCK_DETECT_ENABLED.store(enabled, Ordering::Relaxed);
}

/// 死锁检测是否已打开
pub fn deadlock_detect_enabled() -> bool {
    DEADLOCK_DETECT_ENABLED.load(Ordering::Relaxed)
}
