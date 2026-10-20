//! Process management syscalls
use crate::{
    config::{APP_BASE_ADDRESS, APP_SIZE_LIMIT, MAX_APP_NUM},
    loader::get_user_stack_range,
    task::{
        current_task_id, exit_current_and_run_next, get_current_syscall_count,
        suspend_current_and_run_next,
    },
    timer::get_time_us,
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("[kernel] Application exited with code {}", exit_code);
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// get time with second and microsecond
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    unsafe {
        *ts = TimeVal {
            sec: us / 1_000_000,
            usec: us % 1_000_000,
        };
    }
    0
}

/// 读用户内存 / 写用户内存 / 查某个 syscall 的调用次数
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace {}", trace_request);
    // 0 读、1 写、2 查次数
    match trace_request {
        0 => {
            if !is_user_addr(id) {
                return -1;
            }
            unsafe { (id as *const u8).read_volatile() as isize }
        }
        1 => {
            if !is_user_addr(id) {
                return -1;
            }
            unsafe { (id as *mut u8).write_volatile(data as u8) };
            0
        }
        2 => get_current_syscall_count(id) as isize,
        _ => -1,
    }
}

/// 地址是否落在当前任务能访问的范围里
fn is_user_addr(addr: usize) -> bool {
    let app_end = APP_BASE_ADDRESS + MAX_APP_NUM * APP_SIZE_LIMIT;
    if (APP_BASE_ADDRESS..app_end).contains(&addr) {
        return true;
    }
    let (stack_bottom, stack_top) = get_user_stack_range(current_task_id());
    (stack_bottom..stack_top).contains(&addr)
}
