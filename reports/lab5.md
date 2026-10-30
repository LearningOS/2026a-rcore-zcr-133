# 实验五：并发与同步

## 一、实验目的

第八章的模板把线程创建、互斥锁、信号量、条件变量都写好了，留给实验的内容是死锁检测：让内核在任务将要陷入死锁时把它拦下来，而不是让一组线程互相等下去。

实验要求具体是三个测试程序的表现：一个线程对同一个阻塞互斥锁连锁两次时要拿到 `-0xdead`；三个线程按环形依赖争抢信号量时至少要有一个线程被拒绝；四个线程争用两类资源、但依赖关系没有成环时，全部要正常跑完。

## 二、实验环境

WSL2 里的 Ubuntu 22.04，工具链是模板锁定的 nightly-2024-05-02，目标 `riscv64gc-unknown-none-elf`，模拟器 QEMU 8.2.2（源码编译装在 `~/qemu82`）。检查器与用户程序分别固定在 `rCore-Tutorial-Checker` 的 `7d61ec55` 和 `rCore-Tutorial-Test` 的 `a0593662`，本地用仓库自带的 `rcore_grade.py` 同口径跑，`make test CHAPTER=8` 是实际执行的动作。

## 三、本章的检查点

先读检查器的表比读代码快。`check/ch8.py` 的期望表是 `EXPECTED_6b + 11` 条，再把 `Test sbrk almost OK!` 剔掉，加两条反向检查，一共 25 个点。和第五章、第六章相比换了一套：不再检查 trace、mmap、spawn、set_priority、fstat，而是检查线程与同步原语的用户程序，外加本实验的三条 deadlock。

翻 `ch8_usertest` 的测试列表能确认这一点，因为它就是内核实际会跑的那批程序：ch2b 系列、ch3b 系列、ch5b 的 fork 系列、ch6b 的 file、ch7b 的 pipe，再是 ch8 的 deadlock 三个和 ch8b 的线程、信号量、条件变量、哲学家就餐。所以本章真正被调用、且决定分数的待填点，只有 `sys_enable_deadlock_detect`，另外 `sys_get_time` 是隐式必需（下面第四节末尾会说明原因）。其余几处 `YOUR JOB` 在这一章的测试集合里没有程序会调到。

## 四、死锁检测怎么设计

模板里的两个阻塞原语已经维护了等待队列：`MutexBlockingInner` 有 `wait_queue`，`SemaphoreInner` 也有。但只有等待队列做不了检测，因为我们还需要反过来知道「这份资源现在在谁手里」。所以在 `MutexBlockingInner` 里加了 `holder`，在 `SemaphoreInner` 里加了 `holders`（一个信号量可能有多份实例被不同任务各拿一份），成功获取时记上，释放时按地址比对消掉一个。

检测本身用的是等待图。图的节点是任务，标识直接取 TCB 的地址（`Arc::as_ptr`），边 `a -> b` 表示任务 a 正在等任务 b 让出资源。每次一个任务将要阻塞在锁或信号量上时，先把它这一轮的「我等谁」当成临时边加进图里，然后从这些被等的任务出发做一次深度优先搜索，只要能回到自己，就说明这次阻塞会把图闭合成一个环，直接返回失败而不进入阻塞。拿到资源或者退出时，把任务从图里删掉。

拿这个规则回头看三个测试就清楚了。互斥锁那个是最简单的情形：持有者就是它自己，`blocking_on` 里含有它自己的地址，第一步就判定成环。信号量的环形依赖里，前两个线程各自阻塞时都还不成环，等到第三个线程请求时，三条边闭合，dfs 从它等的持锁者一路走回自己。没有环的那个测试里，四个线程两两分组，组内一个持有、一个等待，谁也没绕回来，于是全部放行。

为了让 syscall 层能把结果如实告诉用户程序，`Mutex::lock` 与 `Semaphore::down` 的返回值从 `()` 改成了 `bool`，`sys_mutex_lock` / `sys_semaphore_down` 在拿到 `false` 时返回 `-0xdead`。信号量那条路径还要多做一步：`down` 是先减 `count` 再判断的，一旦决定拒绝，必须把这个计数加回去，否则资源凭空少一份。

检测的整体开关是 `sync` 模块里的一个原子布尔，`sys_enable_deadlock_detect` 只负责把它置位。这样没打开检测的章节行为不变，`philosopher`、`race_adder` 这些程序也不会因为多了几次图搜索而变慢。

顺带说一句 `sys_get_time`。它不在本章的检查点表里，所以一开始我按「没被调用」处理，把骨架放着没管。跑下来才发现两个信号量测试都卡住，原因在第七节。

## 五、具体改动

几个文件：

- `os/src/sync/deadlock.rs`：新增。等待图的存储（`BTreeMap<任务地址, Vec<任务地址>>`）、判环的 dfs、以及三条维护接口。
- `os/src/sync/mod.rs`：新增 `deadlock` 子模块与一个 `AtomicBool` 开关，配 `set_deadlock_detect` / `deadlock_detect_enabled` 两个函数。
- `os/src/sync/mutex.rs`：trait 的 `lock` 改成返回 `bool`；`MutexBlockingInner` 加 `holder`；锁被阻塞前查环，解锁时把锁和 `holder` 一起转交给被唤醒的任务。
- `os/src/sync/semaphore.rs`：`down` 改成返回 `bool`；`SemaphoreInner` 加 `holders`；获取时登记、释放时按地址消一个，`up` 唤醒等待者时把这份实例记到被唤醒者名下。
- `os/src/syscall/sync.rs`：`mutex_lock` / `semaphore_down` 把失败翻译成 `-0xdead`，`sys_enable_deadlock_detect` 落地。
- `os/src/syscall/process.rs`：实现 `sys_get_time`，用 `translated_byte_buffer` 按页切开写回 `TimeVal`。

内核开了 `deny(warnings)` 和 `deny(missing_docs)`，所以新增的 `pub` 项都得配文档注释，编译才过。

## 六、运行结果

本地跑 `make test CHAPTER=8`，检查器最后一行是 `Test passed64503: 25/25`（`passed` 后面的数字每次随机），随后五份报告的检查全部返回 `found`，`make` 正常退出。三个死锁程序各自的打印都在：`deadlock test mutex 1 OK!`、`deadlock test semaphore 1 OK!`、`deadlock test semaphore 2 OK!`；两条反向检查（不该出现的 `FAIL: T.T` 与 `Test sbrk failed!`）也确实没出现。

作为对照，模板原样跑出来是 22/25，缺的就是这三条 deadlock。

## 七、遇到的问题

第一个问题是 holders 记重了。`Semaphore::down` 阻塞被唤醒之后，我又在新线程上下文里把这份实例记了一遍，而 `up` 在唤醒时已经记过。结果每唤醒一次就多一条过期记录，单个信号量的 `holders` 长度一路涨到四十几个。检测本身不会崩，但等待图里全是已经退出的任务地址，判环的结论就不可信了。改法是删掉 `down` 里那一句，承认「实例归属」这件事只在 `up` 里发生一次。

第二个问题花了最久：实现完检测后互斥锁那条过了，两个信号量测试还是失败，日志里连 `sleep` 都没出现。先做对照实验，把开关临时关掉再跑，结果一模一样，说明和检测无关，于是范围缩到「信号量测试的主线程跑到哪一步消失了」。

这一步用打印定位。在 `sys_thread_create` 的入口、中段、出口各打一行，确认三次线程创建都完整返回；在时钟中断的 `suspend_current_and_run_next` 里对主线程打印 `sepc`，发现它一直停在用户态地址 0x760。把 `ch8_deadlock_sem1` 的 ELF 反汇编出来看那个地址，是一条 `j 0x746` 跳回自己的指令，往回几行是 `li a7, 0x7c`（yield）和 `ecall`，再往前是 `jalr` 调用时间函数、`bge a0, s0` 比较截止时间。

这才明白用户库里的 `sleep` 不是走 101 号 syscall，而是「读时间、不到就 yield、再读」的轮询循环，截止时间靠 `gettimeofday` 往前推。而 `sys_get_time` 在这一章还没实现，一直返回 `-1`，`now` 永远小于 `deadline`，主线程就在这个用户态循环里转到超时。把它按页写回 `TimeVal` 之后就通了。这条也说明「哪些 `YOUR JOB` 不用管」不能只看检查点表，得看用户程序实际调用哪些 syscall。

## 八、小结

死锁检测落在两个地方：资源一侧记「谁持有」，任务一侧记「谁在等谁」，阻塞之前做一次环检测，成环就拒绝而不睡。这套做法对三种典型的争用形态（自锁、环形等待、无环争用）都能给出正确结论，代价是每次阻塞前多一次图搜索。

另外这一章确认了一件事：模板留空的系统调用未必都被本章的检查点覆盖，但可能被用户库间接依赖，判断依据应该是用户程序的实际调用，而不是检查器期望表。
