# 实验一：系统调用 trace

## 一、实验目的

第 3 章把内核从批处理改成多道程序兼分时任务，每个应用成了独立的 task，靠时钟中断轮转。这一章的实验是在这个基础上加一个 `sys_trace` 系统调用（410 号），让任务能观察自己的执行情况：读一个字节、写一个字节、查某个系统调用被调了几次。

仓库里整章只有一处 `// TODO: implement the syscall`，在 `os/src/syscall/process.rs` 的 `sys_trace` 上，syscall 号和 `match` 分支骨架都已经给好了，剩下的是函数体和它需要的数据结构。

## 二、实验环境

WSL2 里的 Ubuntu 22.04.5。工具链用章节仓库 `rust-toolchain.toml` 锁的 nightly-2024-05-02，目标 `riscv64gc-unknown-none-elf`，模拟器 `qemu-system-riscv64` 6.2.0，bootloader 用仓库自带的 rustsbi。用户程序是 rCore-Tutorial-Test 的 `a0593662` 版本，本地评测跑仓库自带的 `.github/scripts/rcore_grade.py`，它内部用 Checker `7d61ec55`，和课程 CI 是同一套检查器。

## 三、接口约定

测试程序 `ch3_trace` 没有直接写内联汇编，走的是 `user_lib` 的四个封装：

```rust
pub fn trace(request: TraceRequest, id: usize, data: usize) -> isize {
    sys_trace(request as usize, id, data)
}

pub fn trace_read(addr: *const u8) -> Option<u8> {
    match trace(TraceRequest::Read, addr as usize, 0) {
        -1 => None,
        data => Some(data as u8),
    }
}

pub fn trace_write(addr: *const u8, data: u8) -> isize {
    trace(TraceRequest::Write, addr as usize, data as usize)
}

pub fn count_syscall(id: usize) -> isize {
    trace(TraceRequest::Syscall, id, 0)
}
```

返回值的约定从这几个封装就能推出来：读失败返回 -1（封装把它变成 `None`），写成功返回 0，查计数直接返回次数。三个动作靠第一个参数区分，`TraceRequest` 是 `repr(C)` 的枚举，Read、Write、Syscall 依次对应 0、1、2。

## 四、计数放在哪儿

这部分是本实验唯一要想一下的地方。第一次读断言时，我奇怪为什么第一次查自己就要求等于 2，而不是 1。测试开头是这两行：

```rust
assert_eq!(2, count_syscall(SYSCALL_TRACE));
...
assert_eq!(7, count_syscall(SYSCALL_TRACE));
```

第一次查 trace 自己的次数，期望值是 2 而不是 1；后面那次 7 也一样，是「之前 6 次 + 当前这次」。也就是说，本次调用本身要算进去。

如果按直觉把计数写在 `sys_trace` 的函数体里，第一次查出来的就是 1，这两条断言会直接挂掉，输出里那两个和 trace 相关的检查点也就跟着没了。所以计数只能放在 `syscall()` 入口，在 `match` 分发之前。

## 五、实现

### 5.1 TCB 里加一张计数表

第 3 章的 TCB 只有状态和上下文两个字段，任务之间没有任何私有数据。要统计每个任务自己的调用次数，就得给它加一个数组：

```rust
#[derive(Copy, Clone)]
pub struct TaskControlBlock {
    pub task_status: TaskStatus,
    pub task_cx: TaskContext,
    /// 每个系统调用被调用过几次
    pub syscall_count: [u32; MAX_SYSCALL_NUM],
}
```

`MAX_SYSCALL_NUM = 512` 放在 `config.rs`，够覆盖课程里用到的号（最大的是信号量那一组，466 到 473，trace 自己是 410）。

数组元素用 `u32` 而不是 `usize`，是因为 `TASK_MANAGER` 是 `lazy_static`，初始化时 `tasks` 这个 16 元素的数组要在启动栈上先构造一次。启动栈只有 `4096 * 16 = 64 KiB`，用 `usize` 的话 16 × 512 × 8B 正好也是 64 KiB，加上别的局部变量基本就是必爆；`u32` 只占 32 KiB，安全得多。实验里一个任务调某个系统调用最多几十次，`u32` 的容量没有意义。

用定长数组而不是 `HashMap` 的原因更简单：TCB 要 `Copy`，`[TaskControlBlock; MAX_APP_NUM]` 的初始化和拷贝都依赖它。

### 5.2 计数与查询

`syscall()` 里加一行，位置在 `match` 之前：

```rust
pub fn syscall(syscall_id: usize, args: [usize; 3]) -> isize {
    // 先计数再分发，这样 sys_trace 查到的值包含它自己这次
    crate::task::inc_current_syscall_count(syscall_id);
    match syscall_id {
        SYSCALL_WRITE => sys_write(args[0], args[1] as *const u8, args[2]),
        ...
    }
}
```

`TaskManagerInner` 上加两个方法，靠 `current_task` 找正在跑的那个任务。下标用 `get_mut` 和 `get` 而不是直接索引，避免非法 syscall 号（≥512）把内核 panic 掉：

```rust
impl TaskManagerInner {
    fn inc_syscall_count(&mut self, syscall_id: usize) {
        let current = self.current_task;
        if let Some(count) = self.tasks[current].syscall_count.get_mut(syscall_id) {
            *count += 1;
        }
    }

    fn get_syscall_count(&self, syscall_id: usize) -> usize {
        let current = self.current_task;
        self.tasks[current]
            .syscall_count
            .get(syscall_id)
            .copied()
            .unwrap_or(0) as usize
    }
}
```

外面再包一层 `TaskManager` 方法和顶层的 `pub fn`，和文件里 `suspend_current_and_run_next` 那套写法保持一致。地址检查还要用到当前任务编号，所以顺手也暴露了一个 `current_task_id()`。三层包装看着啰嗦，但跟仓库原有的风格一致，别处不用跟着改。

### 5.3 sys_trace 本体

```rust
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
```

读写都用 `read_volatile` / `write_volatile`。这个指针是用户从参数里传进来的裸地址，内核没法靠类型系统保证它有效，`volatile` 至少能保证编译器不会把它当普通内存访问去合并或者重排掉。

### 5.4 用户地址检查

第 3 章还没有页表，用户任务和内核共用同一片物理地址空间，硬件层面没有任何隔离。如果内核照着用户给的地址直接读写，一个恶意程序就能让它把内核数据读出来、或者把内核代码写坏。所以 Read 和 Write 之前都要先判断地址范围：

```rust
fn is_user_addr(addr: usize) -> bool {
    let app_end = APP_BASE_ADDRESS + MAX_APP_NUM * APP_SIZE_LIMIT;
    if (APP_BASE_ADDRESS..app_end).contains(&addr) {
        return true;
    }
    let (stack_bottom, stack_top) = get_user_stack_range(current_task_id());
    (stack_bottom..stack_top).contains(&addr)
}
```

认的范围是两块：应用区 `[0x80400000, 0x80400000 + 16 × 0x20000)`，也就是 `link_app.S` 里链接的那些应用占的窗口；以及当前任务自己的用户栈，`loader.rs` 里 `USER_STACK[app_id]` 那个 4 KiB 数组，为了拿到它的区间新加了一个 `get_user_stack_range`。

测试里两个读地址正好一边一个：`trace_read(&var)` 读局部变量，落在栈上；`trace_read(main as *const _)` 读函数指针，落在代码段。

## 六、运行结果

在 `ch3` 分支根目录跑 `python3 .github/scripts/rcore_grade.py`，脚本会重新编译内核和用户程序、在 QEMU 里把第 3 章的应用全跑一遍，再逐项去匹配输出。检查器会把 `OK`、`passed` 这些标记随机加数字后缀防作弊，这次的后缀是 56022：

```text
[PASS] found <get_time OK56022! (\d+)>
[PASS] found <Test sleep OK56022!>
[PASS] found <current time_msec = (\d+)>
[PASS] found <time_msec = (\d+) after sleeping (\d+) ticks, delta = (\d+)ms!>
[PASS] found <Test sleep1 passed56022!>
[PASS] found <string from task trace test>
[PASS] found <Test trace OK56022!>

Test passed56022: 7/7
```

7 个点全过。QEMU 里 trace 这一段的输出是这样：

```text
current time_msec = 14
time_msec = 114 after sleeping 100 ticks, delta = 100ms!
Test sleep1 passed56022!
string from task trace test
Test trace OK56022!
[kernel] Panicked at src/task/mod.rs:156 All applications completed!
```

最后那行 panic 不用担心，第 3 章内核就是靠它在所有任务结束后把 QEMU 退出掉的。

`ch3_trace` 内部那串断言能过，说明几件事：`get_time` 到 sleep 结束后已经调了 5 次以上（`sleep` 的循环里每次都要读时间、再 yield）；trace 自己的计数从 2 涨到 7，和程序里 trace 的调用次数对得上，计数时机是放在分发之前了；`println!` 之前 WRITE 是 0，之后变成 2，一次 `println!` 在用户库里其实拆成了两次写；`trace_read(&var)` 拿到 111，`trace_write` 写进去以后用 `read_volatile` 能读回同一个值，读写两个分支都作用在了用户地址上。

## 七、遇到的问题

1. 内核 crate 顶上同时开了 `#![deny(warnings)]` 和 `#![deny(missing_docs)]`。新增的 `pub` 字段、方法、常量只要少一个 `///`，编译直接报错而不是 warning。按普通项目的习惯先写实现再补注释，第一步就卡在 `missing_docs` 上了，后来把注释补齐才编过。

2. 计数表的大小是算过才定的。先写的是 `[usize; 512]`，顺手算了一下 16 个任务要 64 KiB，而启动栈本身就只有 64 KiB，`TASK_MANAGER` 初始化时是在栈上把这个数组构造出来再挪进静态区的，多半会直接爆。换成 `u32` 之后占 32 KiB，才放心。

3. 只交代码不给报告是过不了的。第一次跑检查器时 7 个测试点已经全过，但脚本退出码还是非零，日志最后一行写着 `Report for lab1 needed. Add your report to reports/lab1.pdf or reports/lab1.md`。`reports/` 这个目录仓库里本来也没有，得自己建。

4. 环境上两个小毛病。非交互 shell 里直接跑 `make` 会报 `rustup: not found`，因为 cargo 的 PATH 写在 `.bashrc` 里，得用 `bash -lic` 进 shell；另外检查器的 `make test` 会先 `make clean` 再从 `temp-os` 备份恢复，这一来一回会把指向 ext4 缓存的 `os/target` 和 `user/target` 两个软链删掉，每跑完一次都要重新建，不然下次编译会落到 9p 上，慢很多。

## 八、下一步

第 4 章上页表以后，5.4 这段地址检查就该整个删掉。那时候每个任务有独立的 `MemorySet`，`sys_trace` 的读写应该走地址翻译，顺带把读、写权限也校了，不需要再靠固定的地址区间去猜哪里是用户内存。
