//! Process management syscalls
use crate::config::{PAGE_SIZE, TRAP_CONTEXT_BASE};
use crate::mm::{MapPermission, PTEFlags, PageTable, PhysAddr, VirtAddr};
use crate::task::{
    change_program_brk, current_syscall_count, current_user_token, exit_current_and_run_next,
    map_current_area, suspend_current_and_run_next, unmap_current_area,
};
use crate::timer::get_time_us;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// 把用户地址翻成内核能直接访问的物理地址，未映射或权限不符返回 None
fn user_phys_addr(token: usize, va: usize, write: bool) -> Option<usize> {
    let page_table = PageTable::from_token(token);
    let virt_addr = VirtAddr::from(va);
    let pte = page_table.translate(virt_addr.floor())?;
    if !pte.is_valid() {
        return None;
    }
    // 内核页（trampoline 等）没有 U 位，用户态不该碰
    if (pte.flags() & PTEFlags::U) == PTEFlags::empty() {
        return None;
    }
    let allowed = if write {
        pte.writable()
    } else {
        pte.readable()
    };
    if !allowed {
        return None;
    }
    Some(usize::from(PhysAddr::from(pte.ppn())) + virt_addr.page_offset())
}

/// 从用户地址读一个字节
fn user_read_byte(token: usize, va: usize) -> Option<u8> {
    let pa = user_phys_addr(token, va, false)?;
    Some(unsafe { *(pa as *const u8) })
}

/// 往用户地址写一个字节，失败返回 false
fn user_write_byte(token: usize, va: usize, data: u8) -> bool {
    match user_phys_addr(token, va, true) {
        Some(pa) => {
            unsafe {
                *(pa as *mut u8) = data;
            }
            true
        }
        None => false,
    }
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// 把当前时间按 sec + usec 写回用户给的 TimeVal
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    let tv = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };
    let token = current_user_token();
    let addr = ts as usize;
    // TimeVal 可能横跨两页，按字节写，每步都过一遍页表
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &tv as *const TimeVal as *const u8,
            core::mem::size_of::<TimeVal>(),
        )
    };
    for (offset, byte) in bytes.iter().enumerate() {
        if !user_write_byte(token, addr + offset, *byte) {
            return -1;
        }
    }
    0
}

/// 0 读用户地址、1 写用户地址、2 查本任务调该 syscall 的次数
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    let token = current_user_token();
    match trace_request {
        0 => user_read_byte(token, id)
            .map(|byte| byte as isize)
            .unwrap_or(-1),
        1 => {
            if user_write_byte(token, id, data as u8) {
                0
            } else {
                -1
            }
        }
        2 => current_syscall_count(id) as isize,
        _ => -1,
    }
}

/// 在用户地址空间映射 len 字节，prot 的低三位依次是 R/W/X
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!("kernel: sys_mmap");
    if start % PAGE_SIZE != 0 || len == 0 || prot == 0 || prot >= 8 {
        return -1;
    }
    let len = (len + PAGE_SIZE - 1) / PAGE_SIZE * PAGE_SIZE;
    let end = match start.checked_add(len) {
        Some(end) if end <= TRAP_CONTEXT_BASE => end,
        _ => return -1,
    };
    let mut permission = MapPermission::U;
    if prot & 1 != 0 {
        permission |= MapPermission::R;
    }
    if prot & 2 != 0 {
        permission |= MapPermission::W;
    }
    if prot & 4 != 0 {
        permission |= MapPermission::X;
    }
    if map_current_area(start.into(), end.into(), permission) {
        0
    } else {
        -1
    }
}

/// 解除 [start, start + len) 的映射，start 和 len 都要求页对齐
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel: sys_munmap");
    if start % PAGE_SIZE != 0 || len == 0 || len % PAGE_SIZE != 0 {
        return -1;
    }
    let end = match start.checked_add(len) {
        Some(end) => end,
        None => return -1,
    };
    if unmap_current_area(start.into(), end.into()) {
        0
    } else {
        -1
    }
}
/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
