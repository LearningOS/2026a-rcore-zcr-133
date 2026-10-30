# 实验二：地址空间

## 一、实验目的

第 4 章给每个任务配上独立的页表，内核和用户程序不再共用同一片物理地址。上一章 `sys_trace` 里那套「按固定区间猜哪里是用户内存」的检查，到这一章完全不能用。

要做的是补四个系统调用：按新地址空间重做 `sys_get_time` 和 `sys_trace`，另外新写 `mmap`、`munmap`。模板在 `os/src/syscall/process.rs` 里留了四处待填，注释分别是 `// YOUR JOB: get time`、`// TODO: Finish sys_trace`、`// YOUR JOB: Implement mmap`、`// YOUR JOB: Implement munmap`。

## 二、实验环境

WSL2 的 Ubuntu 22.04.5，工具链 nightly-2024-05-02，目标 `riscv64gc-unknown-none-elf`，用户程序 `a0593662`，检查器 `7d61ec55`，评测跑仓库自带的 `.github/scripts/rcore_grade.py`。

模拟器换过一次。模板 10 月 19 日把 bootloader 换成 RustSBI 0.4.1，`os/src/sbi.rs` 的 shutdown 也跟着从 8 号 legacy 调用改成 System Reset 扩展。本机原来那份 `qemu-system-riscv64` 是 6.2.0，加载新固件之后停在 `Boot HART ID` 不再往下走，内核一行日志都没有。按课程 CI 的口径源码编了一份 QEMU 8.2.2（只编 riscv64-softmmu）之后就能正常启动，本地和 CI 的模拟器版本也对上了。

## 三、测试程序想要什么

要求都是从 `user/src/bin/ch4_*.rs` 的断言里读出来的，题目本身没有额外文档。

先说权限。`ch4_mmap0` 用 prot=3 映射一页，逐字节写一遍再读回来。`ch4_mmap1` 把 prot 改成 1（只读）之后去写，`ch4_mmap2` 用 prot=2（只写）去读，两个都期望撞上访存异常被杀掉，程序里最后那句 `Should cause error` 一旦打出来就判失败。第二条有点反直觉：RISC-V 里读位为 0 而写位为 1 是非法组合，硬件不认，读必然 page fault。所以权限要照着 prot 原样翻译，不能替用户把读位补上。

再说参数。`ch4_mmap3` 里四种非法调用都得返回 -1：起始地址没对齐（`start + len + 1`）、prot 为 0、prot 带上第 3 位（`prot | 8`）、以及 `mmap(start - len, len + 1, prot)`。`ch4_unmap2` 管 munmap 那头，`munmap(start, len + 1)` 和 `munmap(start + 1, len - 1)` 也要 -1。

`ch4_unmap` 是唯一需要连起来读的一个。它先映射一页，再映射相邻两页，中间用 `munmap(start, len)` 断开，然后跨着断口重新映射一段，最后整段读写校验。`ch4_trace1` 把地址合法性挨个过了一遍：栈变量能读能写，越界地址、内核地址、映射后又解除的地址都读不到也写不进。

这三处期望值第一眼看是矛盾的。`ch4_mmap3` 里 `mmap(start - len, len + 1, prot)` 要 -1，`ch4_unmap` 里同样的参数却要 0。区别不在参数，而在前一步干了什么：前者前面已经映射过 `start` 那一页，`len + 1` 按页向上取整后正好盖住它，撞上已有映射；后者刚把那一页 munmap 掉，同一个调用就落进空档。所以 mmap 的 len 必须向上取整，映射前还得查一遍冲突。munmap 反过来，4096 + 1 和 4095 都要求 -1，start 和 len 都得是页的整数倍，一个字节都不许差。

## 四、实现

### 4.1 用户地址翻译

内核这章跑在自己的页表里，用户传进来的地址是虚拟地址，不能当指针直接用。凡是碰用户内存的系统调用，都先过一遍下面这个函数：

```rust
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
```

`from_token` 从 satp 里取根页号，`translate` 逐级往下走。拿到页表项以后要看三件事：有没有效、有没有 U 位、这次要的读或写权限给不给。返回的是物理地址加页内偏移，内核页表里整片物理内存恒等映射，可以直接解引用。

U 位那一条不能省。用户页表里不只有用户的东西，trampoline 也映射在最高那一页，那个 PTE 有效、可读可执行，但没有 U 位。省掉这一条，`trace_read(isize::MAX)` 会翻到 trampoline 页上，把它的内容当返回值交出去，而不是返回失败。第一次跑出 15/16 就是它。

单字节读写就是两行：

```rust
fn user_read_byte(token: usize, va: usize) -> Option<u8> {
    let pa = user_phys_addr(token, va, false)?;
    Some(unsafe { *(pa as *const u8) })
}

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
```

### 4.2 重做 sys_get_time

上一章这个函数的返回值写死成 -1。现在要真的把时间填进用户给的 `TimeVal`：

```rust
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
```

题目注释里有一句「如果 TimeVal 被两页劈开怎么办」，说的就是这个。`TimeVal` 两个 `usize` 共 16 字节，指针不贴页尾时整块落在一页里，贴着页尾就有一半在下一页。逐字节翻译最省事，代价是每 8 个字节多一次页表查询，系统调用里这点开销无所谓。用 `translated_byte_buffer` 也能跨页，但那个函数不看写权限，用户递个只读地址进来会写坏页。

### 4.3 重做 sys_trace

```rust
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
```

三个分支的语义没变，只是读写换成了上面那两个函数。计数那套整个搬过来：TCB 里加一个 `[u32; MAX_SYSCALL_NUM]`，长度用 `config.rs` 里模板已经写好的 500，计数点仍在 `syscall()` 的 `match` 之前。`ch3_trace` 那条「第一次查自己要等于 2」的断言这一章照样要过，把计数挪进 `sys_trace` 就差一次。

变的还有 TCB 的位置。上一章是 `[TaskControlBlock; 16]` 定长数组，这章换成 `Vec<TaskControlBlock>` 放在内核堆上，两千字节的计数数组不用再挤启动栈。

### 4.4 mmap

```rust
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
```

prot 低三位对应读写执行，高位不许出现。映射一律带 U 位，否则用户态摸不到。prot=2 时只置 W 不置 R，看着像权限没给全，其实正是 `ch4_mmap2` 要的效果，补上读位反而会挂。

### 4.5 munmap 与区间拆分

`MemorySet` 原来只有 `shrink_to` 和 `append_to`，两个都只动一段区域的尾部，给 `sbrk` 用的。munmap 要能从中间挖洞，新加了一个 `unmap_range`：

```rust
pub fn unmap_range(&mut self, start: VirtAddr, end: VirtAddr) -> bool {
    let (start_vpn, end_vpn) = (start.floor(), end.ceil());
    if start_vpn >= end_vpn {
        return false;
    }
    let index = self.areas.iter().position(|area| {
        area.map_type == MapType::Framed
            && area.vpn_range.get_start() <= start_vpn
            && end_vpn <= area.vpn_range.get_end()
    });
    let index = match index {
        Some(index) => index,
        None => return false,
    };
    {
        let (areas, page_table) = (&mut self.areas, &mut self.page_table);
        for vpn in VPNRange::new(start_vpn, end_vpn) {
            areas[index].unmap_one(page_table, vpn);
        }
    }
    // 之后按四种情况调整 vpn_range：整段、掐头、去尾、拆成两块
}
```

先找一段能完整盖住待解除区间的 `MapArea`，找不到直接返回失败。找到就逐页 `unmap_one`，这一步顺手把 `FrameTracker` 从 `data_frames` 摘掉，物理页也就还给帧分配器了。

剩下的活是改 `vpn_range`，四种情况分别处理。整段解除就从 `areas` 里删掉；解除头部把起点后移；解除尾部把终点前移；挖在中间则用 `data_frames.split_off(&end_vpn)` 把后半段的页框收进一个新的 `MapArea`，原来那个只留前半。最后这一支的键要取 `end_vpn` 而不是 `start_vpn`，取错的话前半段会把后半段的页框也算在自己名下，`data_frames` 里就出现超出 `vpn_range` 的记录。

### 4.6 映射区的冲突检查

```rust
pub fn map_framed_area(
    &mut self,
    start: VirtAddr,
    end: VirtAddr,
    permission: MapPermission,
) -> bool {
    let (start_vpn, end_vpn) = (start.floor(), end.ceil());
    let overlapped = self.areas.iter().any(|area| {
        let (area_start, area_end) = (area.vpn_range.get_start(), area.vpn_range.get_end());
        area_start != area_end && area_start < end_vpn && start_vpn < area_end
    });
    if overlapped {
        return false;
    }
    self.insert_framed_area(start, end, permission);
    true
}
```

`area_start != area_end` 这一条是跳过空区域。`from_elf` 在用户栈顶留了一段长度为 0 的 `MapArea` 给 `sbrk` 往上长，把它的起止当正常区间参与比较的话，任何跨过用户栈顶的 mmap 都会被判成冲突，`ch4_unmap` 那条「断开再接上」的用例会直接返回 -1。

## 五、运行结果

在 `ch4` 分支根目录跑 `python3 .github/scripts/rcore_grade.py`，QEMU 用 8.2.2，检查器这次随机后缀是 14685：

```text
Test 04_1 OK14685!
[kernel] PageFault in application, bad addr = 0x10000000, bad instruction = 0x42e, kernel killed it.
[kernel] PageFault in application, bad addr = 0x10000000, bad instruction = 0x42c, kernel killed it.
Test 04_4 test OK14685!
Test trace_1 OK14685!
Test 04_5 ummap OK14685!
Test 04_6 ummap2 OK14685!
...
string from task trace test

Test trace OK14685!
Test sleep OK14685!
[kernel] Panicked at src/task/mod.rs:183 All applications completed!
```

中间两条 PageFault 是 `ch4_mmap1` 和 `ch4_mmap2` 自己撞出来的，写只读页、读只写页各一次，内核按预期把任务杀掉。检查器要的是这两行后面没跟着 `Should cause error`。

十六个点全过：

```text
Test passed14685: 16/16
Report for lab1 found.
Report for lab2 found.
{"chapter": "ch4", "returncode": 0, "passed": true, "points": "16/16"}
```

第一次跑的时候这篇报告还没写，测试点同样是 16/16，但 `returncode` 是 2，日志尾部写着 `Report for lab2 needed`。补上之后才变成 `passed: true`。

检查点从上一章的 7 个变成 16 个，多出来 5 个是本章的期望输出，另有 4 个是「不该出现的输出」的反向检查。

## 六、遇到的问题

1. 第一次跑是 15/16，缺的只有 `Test trace_1 OK!`，日志停在 `Panicked at src/bin/ch4_trace1.rs:17`，对应 `assert_eq!(None, trace_read(isize::MAX as usize as *const _))`。就是我上面说的 trampoline 页，被当成合法用户地址读回来了。加上 U 位检查才过。

2. 重叠检查里的空区域是写代码时发现的，不是跑出来的。`from_elf` 在用户栈顶留的那个 `MapArea` 长度为 0，如果只写 `area_start < end_vpn && start_vpn < area_end`，跨过用户栈顶的 mmap 一律判冲突。多加一条起止不等的判断把空区域排掉。

3. mmap 和 munmap 对 len 的要求不一样，是从三个测试程序的期望值里对出来的。`ch4_unmap` 那条 4097 字节要 0，`ch4_unmap2` 的 4097 要 -1，看第二遍才意识到两处测的不是同一个调用。

4. 环境上是固件和模拟器版本对不上。新 RustSBI 固件要 QEMU 8.2，本机 6.2.0 停在 `Boot HART ID : 0`，表现为内核零输出、检查器超时后只拿到 4/16。定位的办法是把 `ch3` 分支的旧固件换回去试，旧固件下同一个内核跑得起来，范围就收到模拟器上。补上 QEMU 8.2.2 之后消失。

## 七、下一步

第 5 章要把任务变成真正的进程，加 `fork`、`exec`、`waitpid`。`MemorySet` 得支持克隆，现在这两个新方法都要重看一遍：fork 出来的父子是共享页框还是各复制一份，`FrameTracker` 只有一个所有者，这些在这章还没碰到。如果走共享，`unmap_range` 里「摘掉 `FrameTracker` 就归还物理页」这句就不成立了，得先看引用计数。这是我目前最没把握的地方。
