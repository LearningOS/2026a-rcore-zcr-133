# 实验三：进程管理与 stride 调度

## 一、实验目的

第 5 章的内核已经有进程的概念。`fork` 复制地址空间，`exec` 换掉正在跑的程序，`wait` / `waitpid` 回收退出的子进程，pid 和内核栈由 `RecycleAllocator` 动态分配，任务队列也从固定数组换成了 `VecDeque`。这一章要补两件内核里还没有的东西：一是让一个进程不必先 `fork` 就能直接起一个新程序（`spawn`），二是把先进先出的调度换掉，按优先级分配 CPU 时间。

模板在 `os/src/syscall/process.rs` 里留了五处待填，其中 `sys_get_time`、`sys_mmap`、`sys_munmap` 是上一章做过、这一章要按新框架重新写一遍的，`sys_spawn` 和 `sys_set_priority` 是这一章新增的。

## 二、实验环境

WSL2 的 Ubuntu 22.04.5，工具链 nightly-2024-05-02，目标 `riscv64gc-unknown-none-elf`，用户程序 `a0593662`，检查器 `7d61ec55`，源码编的 QEMU 8.2.2，评测跑仓库自带的 `.github/scripts/rcore_grade.py`。

检查器对第 5 章的口径是 15 项。这个数字比第 4 章的 16 项小，容易误会成少了一项要求，实际是 `ch5.py` 在 `EXPECTED_4` 基础上加了四条新期望（`Test spawn0 OK!`、`Test wait OK!`、`Test waitpid OK!`、`Test set_priority OK!`），同时把三条 trace 相关的检查去掉了。反向检查只剩两条：`FAIL: T.T` 和 `Test sbrk failed!` 都不许出现。

## 三、测试程序想要什么

`ch5_usertest` 是整个章节的入口，它自己不做检查，只是按顺序 `spawn` 一批程序再 `waitpid` 等它们退出，最后跑一次 `ch5_stride`。清单里有第 2、3、4 章的老程序，也有这一章新加的三个。

`ch5_spawn0` 一次性起四十个 `ch5_getpid` 子进程，全部存下 pid，再连续 `wait` 四十次，每次都要拿到正的 pid 且退出码为 0，多调一次 `wait` 则必须返回非正数。这里的要求不光是 `spawn` 能建进程，还得撑得住四十个同时存在的任务：每个进程一份地址空间、一份 8 KiB 内核栈、一个 pid，任何一环按固定上限写死都会在这里露出来。

`ch5_spawn1` 走另一条路。它先 `fork` 一个子进程，让子进程去 `exec`，父进程等结果，期望打出 `Test wait OK!` 和 `Test waitpid OK!`。这条和 `spawn` 的区别很关键：`fork` 出来的子进程是父进程的副本，`exec` 只换程序不换 pid，`spawn` 则是直接按名字新建一个进程，父进程和它之间没有地址空间的复制关系。

`ch5_setprio` 最有信息量，四行断言就把优先级的取值范围定死了：`set_priority(10)` 要回 10，`set_priority(isize::MAX)` 要回 `isize::MAX`，而 0、1、-10 全部要回 -1。返回值不是旧优先级，因为进程初始优先级是 16，如果回旧值第一次就该是 16 而不是 10。所以它回的是刚设进去的那个值，等于告诉调用者设置了没有。合法下限落在 2，从拒绝 1 可以推出来。

`ch5_stride` 系列不产生检查点，但它的存在决定了调度器不能只做对一半。`ch5_stride0` 用 `get_time` 计时，跑满四秒就退出并打出 `count/prio`，如果 `get_time` 坏了它会在里面转到天荒地老，整个 usertest 超时，后面的检查点一个都拿不到。

## 四、实现

### 4.1 stride 的数据放在哪

stride 调度需要每个进程记两个数：已经消耗掉的步数 `stride`，以及优先级 `priority`。两者都放在 `TaskControlBlockInner` 里，和 `program_brk` 这类进程私有的状态并列。新建进程时 `stride` 从 0 开始，优先级给 16 当默认值；`fork` 出来的子进程继承父进程的优先级，但 `stride` 同样从 0 起算，不然一个跑了很久的父进程会把它的历史债务一起传给孩子。

步长的换算写成 `TaskControlBlockInner` 的一个小方法：

```rust
pub fn inc_stride(&mut self) {
    self.stride += (BIG_STRIDE / self.priority).max(1);
}
```

`BIG_STRIDE` 取 `1 << 16`，放在 `config.rs`。除出来的结果要兜一个下限，因为 `set_priority(isize::MAX)` 是合法的，`65536 / isize::MAX` 直接截成 0，那样的进程 stride 永不增长，会把 CPU 一直占着。

### 4.2 把先进先出换成 stride

调度点只有一个，就是 `TaskManager::fetch`，`run_tasks` 里每次要切进程都从它拿。原来是 `ready_queue.pop_front()`，改成先扫一遍挑 `stride` 最小的那个，再用 `VecDeque::remove` 按位置取出来：

```rust
let mut pick = 0;
let mut min_stride = usize::MAX;
for (index, task) in self.ready_queue.iter().enumerate() {
    let stride = task.inner_exclusive_access().stride;
    if stride < min_stride {
        min_stride = stride;
        pick = index;
    }
}
let task = self.ready_queue.remove(pick).unwrap();
task.inner_exclusive_access().inc_stride();
Some(task)
```

累加放在出队那一步，而不是放回队列那一步。两个位置对公平性的影响一样，但出队时累加比较省事：进程可能是被时间片打断再入队的，也可能是自己 `yield` 的，出队是它们唯一的公共路径。

`ready_queue` 是 `VecDeque` 不是 `BinaryHeap`，理由有两个。一是任务数不多，四十个进程的线性扫描在这种规模的实验内核里看不出来；二是 `BinaryHeap` 要 `Ord`，而 `Arc<TaskControlBlock>` 的比较得穿过 `UPSafeCell` 加锁去读 `stride`，一边加锁一边比较很容易在别处死锁。

### 4.3 spawn

`spawn` 和 `fork` 的差别集中在一处：`fork` 复制当前进程的地址空间，`spawn` 按名字从内核镜像里取一段 ELF 从头加载。手里已经有 `get_app_data_by_name` 和 `TaskControlBlock::new`，前者按名字查 ELF 字节，后者从 ELF 建出完整的 TCB，所以 `sys_spawn` 本体很短：

```rust
let path = translated_str(current_user_token(), path);
if let Some(data) = get_app_data_by_name(path.as_str()) {
    let parent = current_task().unwrap();
    let new_task = Arc::new(TaskControlBlock::new(data));
    let pid = new_task.pid.0;
    new_task.inner_exclusive_access().parent = Some(Arc::downgrade(&parent));
    parent.inner_exclusive_access().children.push(new_task.clone());
    add_task(new_task);
    pid as isize
}
```

容易漏掉的是中间两行父子关系的维护。`TaskControlBlock::new` 建出来的进程 `parent` 是 `None`、`children` 是空的，如果只把它塞进就绪队列，进程能跑、能退出，但 `ch5_spawn0` 里那四十次 `wait` 都会返回 -1，因为 `sys_waitpid` 是顺着当前进程的 `children` 找僵尸子进程的。`spawn` 出来的进程必须在建的这一刻就挂到调用者名下。

### 4.4 set_priority

按 `ch5_setprio` 的断言写出来就是三行：

```rust
if prio < 2 {
    return -1;
}
let task = current_task().unwrap();
task.inner_exclusive_access().priority = prio as usize;
prio
```

不重置 `stride`。重置看起来更公平，一个刚被提到高优先级的进程立刻获得大量时间片，但那样等于让 `set_priority` 顺便当了一次 `yield`，把调度状态搅乱。让它保留已有的 stride，新优先级从下一轮开始生效。

### 4.5 从第 4 章搬过来的三个

`sys_get_time`、`sys_mmap`、`sys_munmap` 的语义和第 4 章完全一样，但第 5 章的代码不能照抄。

`sys_get_time` 这版改用 `translated_byte_buffer`。这个函数在 `mm/page_table.rs` 里，把用户地址按页切成若干段可写切片返回，`TimeVal` 压在两页交界上时会自然变成两段，逐段拷过去就行，不用自己算页内偏移：

```rust
let mut buffers = translated_byte_buffer(token, ts as *const u8, bytes.len());
let mut written = 0;
for buffer in buffers.iter_mut() {
    let end = written + buffer.len();
    buffer.copy_from_slice(&bytes[written..end]);
    written = end;
}
```

`sys_mmap` 和 `sys_munmap` 搬到 `MemorySet` 上的两个新方法：`map_framed_area` 做冲突检查再插入，`unmap_range` 逐页解除映射后按整段、掐头、去尾、挖中间四种情况调整 `vpn_range`，挖中间时用 `data_frames.split_off` 把后半段的页框拆出来另立一个 `MapArea`。这两个函数是从第 4 章抄过来的，逻辑一行没改，因为第 5 章的 `MemorySet` 结构没变。

## 五、运行结果

本地评测先删掉上一次留下的 `ci-user/`，脚本检测到它会直接退出：

```bash
cd ~/rcore && rm -rf ci-user && python3 -u .github/scripts/rcore_grade.py
```

跑完是 `Test passed4007: 15/15`，`passed: true`，退出码 0。`4007` 是检查器每次随机加的后缀，用来防照抄日志。

十三项正向检查全部命中，其中这一章新增的四项是 `Test spawn0 OK!`、`Test wait OK!`、`Test waitpid OK!`、`Test set_priority OK!`，其余九项是第 3、4 章留下的老检查点，包括 sleep 系列和 mmap/unmap 那五个。两项反向检查 `FAIL: T.T` 和 `Test sbrk failed!` 都没有出现在输出里。

`ch5_usertest` 的执行顺序值得看一眼。清单里的十八个程序按次序 `spawn`，退出码全是 0，只有 `ch4_mmap1` 和 `ch4_mmap2` 是 -2，这两个是故意去写只读页和读只写页的，被内核按访存异常杀掉，属于预期。跑完这十八个之后再 `spawn` 一次 `ch5_stride`，它拿到的是 pid 18，和刚退出的 `ch5_setprio` 同一个号，`RecycleAllocator` 把回收的 pid 直接给了下一个进程。

报告方面，日志里依次打出 `Report for lab1 found.`、`Report for lab2 found.`、`Report for lab3 found.`，三份都在 `reports/` 下。

## 六、遇到的问题

先是编译期卡的。第 4 章的 `sys_get_time` 里自己写了一个地址翻译函数，靠 `crate::mm::{PageTable, PTEFlags}` 拿页表。第 5 章的 `mm/mod.rs` 把这两个类型写成了私有的 `use page_table::{PTEFlags, PageTable};`，没有 `pub use` 出去，照抄过来直接报 `E0603 struct import is private`。改成用模板导出的 `translated_byte_buffer` 之后两个问题一起消失了，顺带还省掉了手算跨页偏移。

还有一处是推断出来的，不是跑挂出来的。`set_priority` 的合法范围我本来想写成 `prio <= 0` 拒绝，因为负数明显不合法。看着 `ch5_setprio` 里那四行断言才发现 1 也要拒绝，而 2 没有被测到。所以下限只能取 2，拒绝 1 说明它认为优先级 1 和 0 是一个性质的非法值。这个边界在题目里没有文字说明，只能从测试反推。

## 七、下一步

第 6 章会接上文件系统。到那一章，取 ELF 的方式不再是从内核镜像里按名字查一段内嵌数据，`spawn` 里调用 `get_app_data_by_name` 那一行要跟着换，否则新进程起不来。
