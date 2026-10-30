# 实验四：文件系统与 I/O

## 一、实验目的

第 6 章把 easy-fs 接了进来。内核镜像里不再内嵌用户程序，`exec` 改成按文件名从磁盘镜像里读 ELF，进程的 TCB 多了 `fd_table`，`stdin` / `stdout` / `stderr` 占住前三个描述符位置。这一章要补的是文件系统这一层的三个系统调用：`fstat` 查元信息、`linkat` 建硬链接、`unlinkat` 删目录项。

模板还在 `syscall/process.rs` 里留着上一章那五处（`get_time`、`mmap`、`munmap`、`spawn`、`set_priority`），所以这一章实际上有八处要填。

## 二、实验环境

WSL2 的 Ubuntu 22.04.5，工具链 nightly-2024-05-02，目标 `riscv64gc-unknown-none-elf`，用户程序 `a0593662`，检查器 `7d61ec55`，源码编的 QEMU 8.2.2，评测跑仓库自带的 `.github/scripts/rcore_grade.py`。

检查器对第 6 章的口径是 31 项：29 条正向期望加 2 条反向检查。相比第 5 章，新增的四条是 `Test file0 OK!`、`Test fstat OK!`、`Test link OK!`、`Test mass open/unlink OK!`，同时把 `Test set_priority OK!` 去掉了，不再检查优先级那一条。

## 三、测试程序想要什么

`ch6_file0` 走的是最基本的读写路径，打开、写入、关掉、再打开读回来，要求内容一致。它顺带验证了 `spawn` 换成从文件系统加载之后还能跑起来。

`ch6_file1` 只查 fstat 的三个字段：`mode` 必须是 `StatMode::FILE`，`nlink` 必须是 1，返回值必须是 0。程序末尾有一句注释说它不会清理自己建的 `fname1`，建议重建镜像，这是提示上一次运行留下的文件会干扰下一次。

`ch6_file2` 是这一章真正难的一个。它建一个文件，连续 `link` 三次把 `nlink` 顶到 4，然后 `unlink` 掉原来的名字，再用第一个链接名把文件打开读内容，读到的还得是之前写进去的 `Hello, world!`，`nlink` 这时候是 3。最后两个链接也删掉，`nlink` 回到 1，同一次 fstat 拿到的 `dev` 和 `ino` 要和最开始那次相等。末行注释明说「不删 inode 和数据块也可以」，所以 `unlink` 只需要把目录项摘掉，不必回收磁盘空间。

`ch6_file3` 是压力测试，反复 open / unlink，看目录项增删到几百次之后还稳不稳。

## 四、实现

### 4.1 先看 easy-fs 有什么

`easy-fs/src/vfs.rs` 里 `Inode` 的方法只有 `find`、`create`、`ls`、`read_at`、`write_at`、`clear`，没有 `link`、`unlink`，也没有取元信息的接口。也就是说这三个系统调用不能只靠系统调用层拼出来，必须往下改到 easy-fs。

`DiskInode` 的布局是这样的：

```rust
pub struct DiskInode {
    pub size: u32,
    pub direct: [u32; INODE_DIRECT_COUNT],
    pub indirect1: u32,
    pub indirect2: u32,
    type_: DiskInodeType,
}
```

算一下大小：4 + 28 × 4 + 4 + 4 + 1 = 125 字节，结构体对齐到 128 字节的 inode 槽位。也就是说想在磁盘上存一个 `nlink` 字段，剩下的 3 个字节塞不下一个 `u32`。真去加字段会把 `get_disk_inode_pos` 里那套按 128 字节切分的定位逻辑全部打乱，磁盘镜像里已有的目录项也会全部错位。

所以硬链接计数不放在磁盘上，改由内核在内存里维护一张 `inode 号到链接数` 的表，`BTreeMap<u32, u32>` 套在 `UPSafeCell` 里，没查到的算 1。测试是单次运行里比较计数，进程退出时表跟着消失，不影响判分。

### 4.2 从位置反算 inode 号

建硬链接要做的事是「在目录里写一条目录项，指向另一个已经存在的 inode」。`DirEntry::new(name, inode_id)` 要的是 inode 号，而 `Inode` 结构体里只存了 `block_id` 和 `block_offset` 两个位置信息，没有 id。

`EasyFileSystem::get_disk_inode_pos` 是 id 到位置的映射，那么反过来写一个就够用：

```rust
pub fn get_inode_id(&self, block_id: u32, block_offset: usize) -> u32 {
    let inode_size = core::mem::size_of::<DiskInode>();
    let inodes_per_block = (BLOCK_SZ / inode_size) as u32;
    (block_id - self.inode_area_start_block) * inodes_per_block
        + (block_offset / inode_size) as u32
}
```

### 4.3 link 与 unlink

`link` 借用了 `create` 的骨架：先查目录里有没有同名，有就返回 -1，没有就按目录项追加一条。区别是不分配新 inode，目录项里的 id 用的是传入的那个 inode 的。

`unlink` 查的是位置而不是 id：遍历目录项找到名字所在的下标，然后把最后一个目录项搬到这个下标上，再把目录 `size` 减掉一个 `DIRENT_SZ`。这样目录项在磁盘上始终是连续的一段，不用留空洞，`find_inode_id` 那种按 `size / DIRENT_SZ` 算数量的遍历也不用改。

```rust
self.modify_disk_inode(|root_inode| {
    let file_count = (root_inode.size as usize) / DIRENT_SZ;
    if index != file_count - 1 {
        let mut last = DirEntry::empty();
        assert_eq!(root_inode.read_at((file_count - 1) * DIRENT_SZ, last.as_bytes_mut(), &self.block_device), DIRENT_SZ);
        root_inode.write_at(index * DIRENT_SZ, last.as_bytes(), &self.block_device);
    }
    root_inode.size -= DIRENT_SZ as u32;
});
```

### 4.4 让 File 能报元信息

`sys_fstat` 拿到的描述符是 `Arc<dyn File + Send + Sync>`，而 `File` trait 原来只有 `readable` / `writable` / `read` / `write` 四个方法，没有取元信息的路子。加一个带默认实现的方法，`Stdin` 和 `Stdout` 就不用动：

```rust
fn stat(&self) -> Stat {
    Stat::empty()
}
```

`OSInode` 覆盖它，读一次磁盘 inode 判断是不是目录，再查硬链接表。`dev` 统一填 0，测试只要求同一个 inode 两次 fstat 相等。

写回用户地址时按字节拷，`Stat` 有 80 字节，落在两页交界上的情况用 `translated_byte_buffer` 按页切开处理。

### 4.5 把上一章的五处搬过来

`get_time`、`mmap`、`munmap`、`set_priority` 的逻辑和第 5 章一样，`MemorySet` 的 `areas` 字段和 `MapArea` 的结构都没变，两个新增的方法可以直接搬。`spawn` 有一处必须改：第 5 章是从内核镜像里 `get_app_data_by_name` 取 ELF，第 6 章那套已经不用了，改成从文件系统读：

```rust
if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
    let all_data = app_inode.read_all();
    let parent = current_task().unwrap();
    let new_task = Arc::new(TaskControlBlock::new(all_data.as_slice()));
    ...
}
```

`ch5_usertest` 那种按名字起一堆进程的写法，在这一章全部改走文件系统，`spawn` 里少了这一步，检查点里 `Test spawn0 OK!` 那一条就没了。

## 五、运行结果

本地评测同样是先清掉 `ci-user/` 再跑 `rcore_grade.py`：

```bash
cd ~/rcore && rm -rf ci-user && python3 -u .github/scripts/rcore_grade.py
```

结果是 `Test passed23352: 31/31`，`passed: true`，退出码 0，后缀 `23352` 是检查器这一轮随机加的防照抄标记。

这一章新增的四条全部命中：`Test file0 OK!`、`Test fstat OK!`、`Test link OK!`、`Test mass open/unlink OK!`。反向两条 `FAIL: T.T` 和 `Test sbrk failed!` 都没有出现。上一章的老检查点也在里面，包括 `Test spawn0 OK!`（验证 spawn 改成从文件系统读 ELF 之后仍然能起进程）、`Test sbrk almost OK!` 和 mmap 那四条。

日志末尾依次打出 `Report for lab1 found.` 到 `Report for lab4 found.`，四份报告都在 `reports/` 下。

## 六、遇到的问题

`Stat` 结构体里有个 `pad: [u64; 7]` 是私有字段，`sys_fstat` 里想直接构造一个 `Stat` 写出去会编译不过，因为跨模块不能写私有字段的结构体字面量。给 `Stat` 加一个 `empty()` 再逐字段赋值绕过去，比把 `pad` 改成 `pub` 干净，毕竟它本来就只是占位。

另一处是 `DiskInode` 放不下 `nlink`。第一反应是直接加一个 `u32` 字段，数了一下 125 + 4 = 129 才意识到会顶掉对齐，结构体变成 132 字节，`get_disk_inode_pos` 里 `(inode_id % inodes_per_block) * inode_size` 算出来的偏移就全错了，磁盘上已有的文件会读成乱码。这属于改一处崩全盘的地方，所以改成了内存表。

## 七、小结

这一章的难处在 easy-fs 不在内核侧：三个系统调用的语义都很短，但每个都要往下摸到 vfs 层的目录项操作。改完之后 `link` / `unlink` 这套接口留在 easy-fs 里，内核侧只剩参数翻译。
