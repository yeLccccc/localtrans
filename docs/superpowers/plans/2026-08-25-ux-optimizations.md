# UX 优化四项(长按菜单/双进度条/单击直拉/hash 秒传)Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 spec `docs/superpowers/specs/2026-08-25-ux-optimizations-design.md` 的四个优化:文件长按菜单、大文件推送双进度条、远程文件单击直拉、收件箱 hash 秒传。

**Architecture:** 模块 A(纯 Android UI)零协议依赖;模块 B 在 core 新增 `RecvProgress` 逐窗确认消息,经 `SourceSpeed.remote_done` 透传到双壳 TransferDto;模块 C 在 core 新建 `dedup.rs` 收件箱索引,`OfferFile.hash`/`MetaResp.file_hash`/`OfferResp.skip` 三个 serde-default 字段保证新旧对端互通。FFI TransferDto 一次加 `remote_done`+`instant` 两字段,Kotlin 绑定在 B/C 全部合完后只重生成一次。

**Tech Stack:** Rust(tokio/quinn/serde/sha2)、uniFFI 0.28、Kotlin Compose Material3。

## Global Constraints

- PSK 不进日志(服务端只打 sha256 前 8 位摘要)
- 明文文件名/路径不进 tracing 日志(只打指纹/ID/数量);秒传日志只打 hash 前 8 位
- dist zip 不含 data/ 目录(identity.key/cert.der 是私钥)
- 提交信息:中文前缀 + 空行 + `Co-Authored-By: Claude <noreply@anthropic.com>`
- Rust 测试 per-crate `-- --test-threads=1`;FFI 测试 7 个端口占用失败(用户运行实例占用 UDP 47601)为已知非回归
- 工具链:JDK17=`C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8`;Gradle=`C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin`(android/ 无 wrapper);SDK=`C:/Users/<user>/Desktop/work/deepseek_use/tools/android-sdk`;NDK=27.0.12077973
- so 重编:`cargo ndk -t arm64-v8a -t x86_64 -o android/app/src/main/jniLibs build -p localtrans-ffi --release`(需 export ANDROID_NDK_HOME/CARGO_HOME/PATH)
- 版本三处对齐:Cargo.toml `[workspace.package]` version、src-tauri/tauri.conf.json、android/app/build.gradle.kts versionName

---

### Task 1: FileEntryMenuSheet 组件 + 本机"全部"Tab 长按菜单

**Files:**
- Create: `android/app/src/main/java/com/localtrans/app/ui/files/FileEntryMenuSheet.kt`
- Modify: `android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt`(FileEntryCard 664-700 行、本机 ALL Tab 242-255 行)

**Interfaces:**
- Consumes: `FilesViewModel.renameEntry(oldPath: String, newName: String)`、`deleteEntries(paths: List<String>)`、`toggleSelection(path: String)`(FilesViewModel.kt:217-295,已存在)
- Produces: `FileEntryMenuSheet(isRemote, onDismiss, onDownload, onRename, onDelete, onSelectMultiple)` 可复用组件,Task 2 远程页复用

- [ ] **Step 1: 写 FileEntryMenuSheet.kt**

```kotlin
package com.localtrans.app.ui.files

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp

/**
 * 文件项长按菜单(本机/远程通用)。
 * 远程页多一项"下载到本机";"选择多项"进入现有多选模式。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FileEntryMenuSheet(
    isRemote: Boolean,
    onDismiss: () -> Unit,
    onDownload: () -> Unit,
    onRename: () -> Unit,
    onDelete: () -> Unit,
    onSelectMultiple: () -> Unit
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        if (isRemote) {
            MenuRow(icon = Icons.Default.Download, label = "下载到本机", onClick = onDownload)
        }
        MenuRow(icon = Icons.Default.Edit, label = "重命名", onClick = onRename)
        MenuRow(icon = Icons.Default.Delete, label = "删除", onClick = onDelete)
        MenuRow(icon = Icons.Default.Checklist, label = "选择多项", onClick = onSelectMultiple)
        Spacer(modifier = Modifier.height(24.dp))
    }
}

@Composable
private fun MenuRow(icon: ImageVector, label: String, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 24.dp, vertical = 16.dp),
        horizontalArrangement = Arrangement.spacedBy(16.dp)
    ) {
        Icon(icon, contentDescription = null)
        Text(text = label, style = MaterialTheme.typography.bodyLarge)
    }
}
```

- [ ] **Step 2: FileEntryCard 长按接线(combinedClickable)**

FilesScreen.kt:664 的 `FileEntryCard` 现用 `.clickable { onClick() }`(673 行),`onLongClick` 参数未接进 modifier。改为:

```kotlin
// 文件顶部加 import:
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable

// FileEntryCard 函数加注解:
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun FileEntryCard(
    entry: FileEntryUi,
    isSelected: Boolean,
    onClick: () -> Unit,
    onLongClick: () -> Unit
) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .combinedClickable(onClick = onClick, onLongClick = onLongClick),
        // ...colors 等其余不变
```

(其余 ListItem 结构保持不变。)

- [ ] **Step 3: 本机 ALL Tab 加菜单状态与长按弹菜单**

FilesScreen.kt 本机 ALL Tab(242-255 行 items 循环)。在 FilesScreen 顶部状态区(约 99 行附近)加:

```kotlin
    // 长按菜单状态
    var menuEntry by remember { mutableStateOf<FileEntryUi?>(null) }
```

本机 ALL Tab 的 FileEntryCard 调用改为:

```kotlin
items(entries) { entry ->
    FileEntryCard(
        entry = entry,
        isSelected = selectedEntries.contains(entry.path),
        onClick = {
            if (selectedEntries.isNotEmpty()) {
                filesViewModel.toggleSelection(entry.path)
            } else if (entry.isDir) {
                filesViewModel.navigateTo(entry.path)
            }
        },
        onLongClick = { menuEntry = entry }   // 原来是 toggleSelection
    )
}
```

在 FilesScreen 的 Box 之后(与 showRenameDialog 等弹窗同级,约 423 行前)加菜单挂载:

```kotlin
    menuEntry?.let { entry ->
        FileEntryMenuSheet(
            isRemote = false,
            onDismiss = { menuEntry = null },
            onDownload = {},
            onRename = {
                renamePath = entry.path
                renameOldName = entry.name
                menuEntry = null
                showRenameDialog = true
            },
            onDelete = {
                deletePaths = listOf(entry.path)
                menuEntry = null
                showDeleteDialog = true
            },
            onSelectMultiple = {
                filesViewModel.toggleSelection(entry.path)
                menuEntry = null
            }
        )
    }
```

(本机页无下载项,`onDownload = {}` 不显示——`isRemote=false` 时组件不渲染该项。)

- [ ] **Step 4: 编译 + 单测回归**

```bash
cd android && export JAVA_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8" && export PATH="C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin:$PATH" && gradle.bat :app:compileDebugKotlin :app:testDebugUnitTest
```

Expected: BUILD SUCCESSFUL(既有单测全过;无 FFI 改动,无绑定变化)

- [ ] **Step 5: Commit**

```bash
git add android/app/src/main/java/com/localtrans/app/ui/files/FileEntryMenuSheet.kt android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt
git commit -m "feat(android): 文件长按菜单——本机页重命名/删除/选择多项

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 2: 远程页菜单 + 单击文件直拉

**Files:**
- Modify: `android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt`(远程页 337-350 行 FileEntryCard、菜单挂载)
- Test: `android/app/src/test/java/com/localtrans/app/ui/files/FilesViewModelTest.kt`

**Interfaces:**
- Consumes: Task 1 的 `FileEntryMenuSheet`;`FilesViewModel.pullFiles(fp: String, shareId: String, remotePaths: List<String>)`(FilesViewModel.kt:319);`FilesScreen` 的 `onNavigateToTransfers: () -> Unit` 参数(45 行,已存在)
- Produces: 无(终端 UI 任务)

- [ ] **Step 1: 写失败测试(单文件直拉)**

FilesViewModelTest.kt 追加(若 Fake repo 未记录 pull 调用,先在 Fake 里加 `val pulled = mutableListOf<Triple<String, String, List<String>>>()` 并在 pullFiles 实现里记录):

```kotlin
@Test
fun `pull single remote file passes single-element list`() = runTest {
    viewModel.setLocation(Location.REMOTE)
    viewModel.setSelectedDevice("aabb")
    viewModel.pullFiles("aabb", "share1", listOf("/remote/dir/video.mp4"))
    advanceUntilIdle()
    assertEquals(1, fakeRepo.pulled.size)
    assertEquals(listOf("/remote/dir/video.mp4"), fakeRepo.pulled.last().third)
}
```

Run(同 Task 1 环境变量):`gradle.bat :app:testDebugUnitTest --tests "*FilesViewModelTest*"`
Expected: FAIL(Fake 无记录/断言不过)

- [ ] **Step 2: Fake repo 记录 pull 调用,测试转绿**

在测试用 Fake FilesRepo 的 pullFiles 实现里加 `pulled.add(Triple(fp, shareId, remotePaths))`。
Run 同上,Expected: PASS

- [ ] **Step 3: 远程页 FileEntryCard 接线**

FilesScreen.kt 远程页 items(337-350 行)改为:

```kotlin
items(entries) { entry ->
    FileEntryCard(
        entry = entry,
        isSelected = selectedEntries.contains(entry.path),
        onClick = {
            when {
                selectedEntries.isNotEmpty() ->
                    filesViewModel.toggleSelection(entry.path)
                entry.isDir ->
                    filesViewModel.navigateTo(entry.path)
                else -> {
                    // 单击远程文件直拉(一步入口)
                    val fp = uiState.selectedDeviceFp ?: ""
                    val shareId = currentShareId ?: ""
                    if (fp.isNotEmpty() && shareId.isNotEmpty()) {
                        filesViewModel.pullFiles(fp, shareId, listOf(entry.path))
                        coroutineScope.launch {
                            val result = snackbarHostState.showSnackbar(
                                message = "已开始下载 ${entry.name}",
                                actionLabel = "查看",
                                duration = SnackbarDuration.Short
                            )
                            if (result == SnackbarResult.ActionPerformed) {
                                onNavigateToTransfers()
                            }
                        }
                    }
                }
            }
        },
        onLongClick = { menuEntry = entry }
    )
}
```

(需 import `androidx.compose.material3.SnackbarDuration`、`androidx.compose.material3.SnackbarResult`;`menuEntry` 是 Task 1 加的状态。)

- [ ] **Step 4: 远程页菜单挂载(Task 1 的 menuEntry?.let 改为区分远程)**

把 Task 1 的 `menuEntry?.let { ... }` 块整体替换为:

```kotlin
    menuEntry?.let { entry ->
        val isRemotePage = uiState.location == Location.REMOTE
        FileEntryMenuSheet(
            isRemote = isRemotePage,
            onDismiss = { menuEntry = null },
            onDownload = {
                val fp = uiState.selectedDeviceFp ?: ""
                val shareId = shares.firstOrNull()?.shareId
                    ?: uiState.selectedShareId ?: ""
                if (fp.isNotEmpty() && shareId.isNotEmpty()) {
                    filesViewModel.pullFiles(fp, shareId, listOf(entry.path))
                }
                menuEntry = null
            },
            onRename = {
                renamePath = entry.path
                renameOldName = entry.name
                menuEntry = null
                showRenameDialog = true
            },
            onDelete = {
                deletePaths = listOf(entry.path)
                menuEntry = null
                showDeleteDialog = true
            },
            onSelectMultiple = {
                filesViewModel.toggleSelection(entry.path)
                menuEntry = null
            }
        )
    }
```

- [ ] **Step 5: 编译 + 全部单测**

`gradle.bat :app:compileDebugKotlin :app:testDebugUnitTest`
Expected: BUILD SUCCESSFUL

- [ ] **Step 6: Commit**

```bash
git add android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt android/app/src/test/java/com/localtrans/app/ui/files/FilesViewModelTest.kt android/app/src/test/java/com/localtrans/app/ui/files/FakeFilesRepo.kt
git commit -m "feat(android): 远程页长按菜单+单击文件直拉下载

Co-Authored-By: Claude <noreply@anthropic.com>"
```

(Fake 文件名以实际为准,`git status` 确认。)

---

### Task 3: core 协议——RecvProgress 消息 + 发送侧管线

**Files:**
- Modify: `crates/localtrans-core/src/protocol.rs`(ControlMsg 加 RecvProgress)
- Modify: `crates/localtrans-core/src/session.rs`(1312-1317 入站路由分类)
- Modify: `crates/localtrans-core/src/transfer/engine.rs`(ProgressEvent::SourceSpeed 加字段 387、router 新分支 ~2001、probe 启动点 1700)
- Modify: `crates/localtrans-core/src/transfer/sender_state.rs`(SenderJobState 加字段)
- Modify: `crates/localtrans-core/src/transfer/source_probe.rs`(SourceSpeed 构造点 74)
- Test: `crates/localtrans-core/src/protocol.rs` 既有 serde 测试区

**Interfaces:**
- Consumes: 无(协议层起点)
- Produces: `ControlMsg::RecvProgress { job_id: u64, cumulative_bytes: u64 }`;`ProgressEvent::SourceSpeed` 增 `remote_done: u64` 字段(Task 4/5 消费);`SenderJobState.remote_done: Arc<AtomicU64>`

- [ ] **Step 1: 写失败测试(协议 roundtrip)**

protocol.rs 测试模块(360 行附近的 roundtrip 测试旁)加:

```rust
#[test]
fn recv_progress_roundtrip() {
    let m = ControlMsg::RecvAck { job_id: 42 };
    let _ = m; // 既有锚

    let msg = ControlMsg::RecvProgress { job_id: 7, cumulative_bytes: 123456 };
    let buf = encode_control(&msg);
    let decoded = decode_control(&mut std::io::Cursor::new(&buf)).unwrap();
    assert_eq!(decoded, msg);
}
```

(decode 函数名以 protocol.rs 现有测试用法为准——先看 360 行附近既有测试怎么调 decode,照抄调用形式。)

Run:`cargo test -p localtrans-core recv_progress_roundtrip -- --test-threads=1`
Expected: FAIL(RecvProgress 不存在,编译错误)

- [ ] **Step 2: 加协议消息 + session 路由**

protocol.rs ControlMsg 里 RecvAck 变体(138-140 行)之后加:

```rust
    /// v0.10.0 推送模式逐窗接收进度(接收方每收完一个块窗口回发;
    /// 发送方据此在 UI 呈现"对方已收"第二条进度。旧端收到按未知消息忽略)
    RecvProgress {
        job_id: u64,
        cumulative_bytes: u64,
    },
```

session.rs 1312-1317 的 match 臂(RecvAck 所在的 inbound_ctrl 转发类)加一行:

```rust
                                    ControlMsg::MetaReq { .. } |
                                    ControlMsg::FetchReq { .. } |
                                    ControlMsg::OfferReq { .. } |
                                    ControlMsg::BitmapReq { .. } |
                                    ControlMsg::TransferCtl { .. } |
                                    ControlMsg::RecvProgress { .. } |
                                    ControlMsg::RecvAck { .. } => {
```

(注意:此 match 有兜底臂,漏加不会编译报错、只会静默丢弃——必须显式加。)

Run Step 1 测试,Expected: PASS

- [ ] **Step 3: SourceSpeed 事件与 SenderJobState 加字段**

engine.rs 387-394:

```rust
    SourceSpeed {
        job_id: u64,
        bps: u64,
        loss_ratio: f64,
        rtt_ms: u64,
        cwnd: u64,
        streams: u32,
        /// v0.10.0 对端累计已确认字节(RecvProgress 驱动;0=对端未上报,UI 降级隐藏)
        remote_done: u64,
    },
```

sender_state.rs:13 SenderJobState 加字段(34 行 offer_id 后):

```rust
    /// v0.10.0 对端累计已收字节(RecvProgress 更新,source_probe 500ms 读取)
    pub remote_done: Arc<AtomicU64>,
```

构造函数 new_sender_job_state(47-60)加 `remote_done: Arc::new(AtomicU64::new(0)),`。

source_probe.rs:run_source_probe 签名加参数 `remote_done: Arc<std::sync::atomic::AtomicU64>`,74-83 构造点加 `remote_done: remote_done.load(Ordering::Relaxed),`。

engine.rs 1700-1707 spawn 点(probe_stop_rx 前)加传参 `state.remote_done.clone(),`。

- [ ] **Step 4: router 消费 RecvProgress**

engine.rs 2001 `ControlMsg::RecvAck { job_id }` 分支**之前**加:

```rust
                ControlMsg::RecvProgress { job_id, cumulative_bytes } => {
                    // v0.10.0 双进度:更新 sender 任务的 remote_done,source_probe
                    // 500ms 周期随 SourceSpeed 带出(不额外发事件,防洪泛)
                    if let Some(state) = sender_jobs.read().await.get(&job_id) {
                        state.remote_done.store(
                            cumulative_bytes,
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }
                }
```

- [ ] **Step 5: 编译 + core 全量回归**

```bash
cargo test -p localtrans-core -- --test-threads=1
```

Expected: 编译过;既有测试全绿(SourceSpeed 加字段只影响构造点 source_probe 一处,消费点都是 `..` 解构)

- [ ] **Step 6: Commit**

```bash
git add crates/localtrans-core/src/protocol.rs crates/localtrans-core/src/session.rs crates/localtrans-core/src/transfer/engine.rs crates/localtrans-core/src/transfer/sender_state.rs crates/localtrans-core/src/transfer/source_probe.rs
git commit -m "feat(core): RecvProgress 逐窗确认消息+发送侧 remote_done 管线

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 4: core 接收侧发射 + 双进度 E2E

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs`(recv_push_large_file 1133-1231)
- Test: `crates/localtrans-core/src/transfer/engine.rs` 测试模块(2980 行附近既有大文件推送 E2E 旁)

**Interfaces:**
- Consumes: Task 3 的 `ControlMsg::RecvProgress`
- Produces: 接收侧发射行为(每块窗口一批 RecvProgress);E2E 测试 `push_large_file_reports_remote_done`(Task 5/6 依赖的行为契约)

- [ ] **Step 1: 写失败 E2E**

参照 2980 行既有 `乙收到大文件后回 RecvAck` E2E 的两设备搭建模式(mk_ctx/make_peer/trust 互信/push_files_rel 调用),新测试核心断言:

```rust
    #[tokio::test]
    async fn push_large_file_reports_remote_done() {
        // 搭建同既有 E2E:两 ctx 互信、share 注册、>1MiB 文件写入
        // 甲侧 spawn_rpc_router 带 source_event_tx(收集发送方事件)
        // 甲 push_files_rel(乙, vec![(大文件路径, "".into())], ...)
        // 收集 SourceSpeed 事件直到 SourceDone:
        let speeds: Vec<u64> = events中按序取 SourceSpeed.remote_done;
        // 断言:单调不减、最后一个 == 文件大小
        assert!(speeds.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(*speeds.last().unwrap(), FILE_SIZE);
    }
```

(两设备搭建与事件收集的具体写法照抄 2980 行测试——它已验证 SourceStarted/SourceDone 闭环;本测试只是把收集范围扩到 SourceSpeed。文件大小用 >1MiB(如 5 * 1024 * 1024 + 123)走大文件路径。)

Run:`cargo test -p localtrans-core push_large_file_reports_remote_done -- --test-threads=1`
Expected: FAIL(remote_done 恒 0,最后值 != 文件大小)

- [ ] **Step 2: 接收侧发射实现**

engine.rs recv_push_large_file:1205-1215 窗口循环改为(创建累计计数器并传入 run_receiver,每窗口完成后回发):

```rust
    let writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer));

    // v0.10.0 双进度:接收侧累计字节计数器,每收完一个窗口回发 RecvProgress
    let cumulative = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

    // 2. 窗口化 FetchReq + 收块流(沿用拉取主链路的固定窗口;乙侧自适应归 T16 UI 集成)
    let mut idx = 0;
    while idx < missing.len() {
        let window = 4usize;
        let end = (idx + window).min(missing.len());
        let batch = &missing[idx..end];
        run_receiver(conn, pool, sm, peer, job_id, &writer, batch, progress, Some(cumulative.clone())).await?;
        idx = end;
        // v0.10.0:整窗落盘后回发累计字节(4MB/块 × 4 块/窗 ≈ 每 16MB 一条,无洪泛)
        let _ = sm.send_ctrl(peer, ControlMsg::RecvProgress {
            job_id,
            cumulative_bytes: cumulative.load(std::sync::atomic::Ordering::Relaxed),
        }).await;
    }
```

(原 1213 行 `run_receiver(..., None)` 的 None 即 counter 参数——替换为 `Some(cumulative.clone())`;run_receiver 内部 1417 行 `counter.fetch_add` 自动累计,无需改 run_receiver。)

- [ ] **Step 3: 跑 E2E 转绿 + 全量回归**

```bash
cargo test -p localtrans-core push_large_file_reports_remote_done -- --test-threads=1
cargo test -p localtrans-core -- --test-threads=1
```

Expected: 新测试 PASS;全量回归无新增失败(若推送路径既有测试因多出 Speed 事件断言失败,按实际事件流修测试断言——不是砍实现)

- [ ] **Step 4: Commit**

```bash
git add crates/localtrans-core/src/transfer/engine.rs
git commit -m "feat(core): 接收侧逐窗回发 RecvProgress——双进度条数据源

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 5: FFI TransferDto.remote_done

**Files:**
- Modify: `crates/localtrans-ffi/src/dto.rs`(TransferDto 17-33)
- Modify: `crates/localtrans-ffi/src/lib.rs`(SourceSpeed 分支 2191;全部 TransferDto 字面量;TransferRecord 持久化结构)
- Test: `crates/localtrans-ffi/src/lib.rs` 测试模块

**Interfaces:**
- Consumes: Task 3 的 `ProgressEvent::SourceSpeed.remote_done`
- Produces: `TransferDto.remote_done: u64`、`TransferDto.instant: bool`(instant 本任务加字段占位默认 false,Task 9/10 写真值——避免二次绑定重生成)

- [ ] **Step 1: 写失败测试**

lib.rs 测试模块加:

```rust
    #[tokio::test]
    async fn source_speed_updates_remote_done() {
        let (st, cb) = test_app_state();
        // 先建一行(push 方向)
        st.transfer_update(1, crate::dto::TransferDto {
            job_id: 1, name: "big.bin".into(), total: 100, done: 40,
            state: "active".into(), speed_bps: 0, peer: "aa".into(),
            direction: "push".into(), local_role: "source-push".into(),
            progress_percent: 40, eta_secs: -1, fail_reason: String::new(),
            local_path: None, remote_done: 0, instant: false,
        }).await;
        handle_source_progress_event(&st, &cb, localtrans_core::transfer::ProgressEvent::SourceSpeed {
            job_id: 1, bps: 1024, loss_ratio: 0.0, rtt_ms: 10, cwnd: 100, streams: 2,
            remote_done: 60,
        }).await;
        let dto = st.transfer_get_mut(1).await.unwrap();
        assert_eq!(dto.remote_done, 60);
        assert_eq!(dto.speed_bps, 1024);
    }
```

(test_app_state/handle_source_progress_event 的实际签名以测试模块既有测试的调用形式为准照抄。)

Run:`cargo test -p localtrans-ffi source_speed_updates_remote_done -- --test-threads=1`
Expected: FAIL(编译错:TransferDto 无 remote_done 字段)

- [ ] **Step 2: 加字段 + 全字面量补默认值**

dto.rs TransferDto 在 `local_path` 后加:

```rust
    /// v0.10.0 push 方向:对端累计已确认字节(RecvProgress 驱动;0=对端未上报)
    pub remote_done: u64,
    /// v0.10.0 秒传命中标记(接收侧 InstantHit 置 true;done+instant 显示"秒传"徽标)
    pub instant: bool,
```

lib.rs 里**所有** TransferDto 字面量(编译器逐个报错)补 `remote_done: 0, instant: false,`(SourceSpeed 分支处写真值)。2191 分支改:

```rust
        localtrans_core::transfer::ProgressEvent::SourceSpeed { job_id, bps, remote_done, .. } => {
            if let Some(mut dto) = st.transfer_get_mut(job_id).await {
                dto.speed_bps = bps;
                dto.remote_done = remote_done;
                // ...(eta/节流逻辑原样保留)
```

TransferRecord( transfers.json 持久化结构)加对应字段:

```rust
    #[serde(default)]
    pub remote_done: u64,
    #[serde(default)]
    pub instant: bool,
```

两处 From 互转补字段映射(`r.remote_done`↔`d.remote_done`、`r.instant`↔`d.instant`)。

- [ ] **Step 3: 跑测试转绿 + FFI 回归**

```bash
cargo test -p localtrans-ffi source_speed_updates_remote_done -- --test-threads=1
cargo test -p localtrans-ffi -- --test-threads=1
```

Expected: 新测试 PASS;除已知 7 个端口占用外无新增失败

- [ ] **Step 4: Commit**

```bash
git add crates/localtrans-ffi/src/dto.rs crates/localtrans-ffi/src/lib.rs
git commit -m "feat(ffi): TransferDto 加 remote_done/instant——双进度与秒传 UI 数据位

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 6: PC 壳 TransferDto.remote_done

**Files:**
- Modify: `src-tauri/src/main.rs`(TransferDto 253-281、SourceSpeed 分支 1157-1163)

**Interfaces:**
- Consumes: Task 3 的 `SourceSpeed.remote_done`
- Produces: PC TransferDto 新字段(serde default,旧 transfers.json 兼容;PC 前端显示为 backlog 不做)

- [ ] **Step 1: 加字段**

main.rs TransferDto 在 `fail_reason` 后加:

```rust
    /// v0.10.0 push 方向:对端累计已确认字节(前端显示 backlog,先落字段)
    #[serde(default)]
    remote_done: u64,
    /// v0.10.0 秒传命中标记
    #[serde(default)]
    instant: bool,
```

- [ ] **Step 2: SourceSpeed 分支写真值 + 字面量补默认**

1157 分支:

```rust
                            PE::SourceSpeed { job_id, bps, remote_done, loss_ratio, rtt_ms, cwnd, streams } => {
                                if let Some(mut dto) = st.transfer_get_mut(job_id).await {
                                    dto.speed_bps = bps;
                                    dto.remote_done = remote_done;
                                    dto.health = Some(HealthDto { loss_ratio, rtt_ms, cwnd, streams });
```

其余 TransferDto 字面量逐个补 `remote_done: 0, instant: false,`。

- [ ] **Step 3: 编译 + 回归**

```bash
cargo build -p localtrans --release 2>&1 | tail -5
cargo test -p localtrans-ffi -- --test-threads=1 2>&1 | tail -5
```

(crate 名以 workspace 实际为准——src-tauri 的 package 名;编译过即可,PC 无独立测试面)

Expected: 编译通过

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/main.rs
git commit -m "feat(pc): TransferDto 加 remote_done/instant 字段(serde default 兼容旧档)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 7: dedup.rs 收件箱索引(TDD)

**Files:**
- Create: `crates/localtrans-core/src/transfer/dedup.rs`
- Modify: `crates/localtrans-core/src/transfer/mod.rs`(加 `pub mod dedup;`)

**Interfaces:**
- Consumes: 无
- Produces: `InboxIndex`(load/lookup/insert/save)、`place_dedup_copy(src, dest_dir, file_name) -> io::Result<PathBuf>`、`sha256_file(path) -> io::Result<String>`——Task 8/9 消费

- [ ] **Step 1: 写失败测试(dedup.rs 底部测试模块)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_or_corrupt_returns_empty() {
        let dir = tempdir().unwrap();
        let idx = InboxIndex::load(&dir.path().join("idx.json"));
        assert!(idx.lookup("deadbeef", 1).is_none());
        std::fs::write(dir.path().join("idx.json"), "{not json").unwrap();
        let idx2 = InboxIndex::load(&dir.path().join("idx.json"));
        assert!(idx2.lookup("deadbeef", 1).is_none());
    }

    #[test]
    fn lookup_hit_requires_size_match() {
        let dir = tempdir().unwrap();
        let mut idx = InboxIndex::load(&dir.path().join("idx.json"));
        idx.insert("h1".into(), dir.path().join("a.bin"), 100);
        assert_eq!(idx.lookup("h1", 100), Some(&dir.path().join("a.bin")));
        assert_eq!(idx.lookup("h1", 101), None, "size 不符视为未命中");
        assert_eq!(idx.lookup("h2", 100), None);
    }

    #[test]
    fn insert_then_save_reload_roundtrip() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("idx.json");
        {
            let mut idx = InboxIndex::load(&p);
            idx.insert("h1".into(), dir.path().join("a.bin"), 7);
            idx.save();
        }
        let idx2 = InboxIndex::load(&p);
        assert_eq!(idx2.lookup("h1", 7), Some(&dir.path().join("a.bin")));
    }

    #[test]
    fn place_copy_hardlink_or_copy_and_conflict_rename() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.bin");
        std::fs::write(&src, b"hello").unwrap();
        // 目标已有同名文件 → 冲突重命名 name (1).ext
        std::fs::write(dir.path().join("out").join("src.bin"), b"old").unwrap();
        let dest = place_dedup_copy(&src, &dir.path().join("out"), "src.bin").unwrap();
        assert_eq!(dest.file_name().unwrap().to_str().unwrap(), "src (1).bin");
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        assert_eq!(std::fs::read(dir.path().join("out").join("src.bin")).unwrap(), b"old");
    }

    #[test]
    fn place_copy_same_path_guard() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("same.bin");
        std::fs::write(&src, b"x").unwrap();
        let dest = place_dedup_copy(&src, dir.path(), "same.bin").unwrap();
        assert_eq!(dest, src); // 源即目标:不复制
    }

    #[test]
    fn sha256_file_known_vector() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
```

Run:`cargo test -p localtrans-core dedup -- --test-threads=1`
Expected: FAIL(模块不存在,编译错)

- [ ] **Step 2: 实现 dedup.rs**

```rust
//! v0.10.0 收件箱 hash 秒传:下载目录的已收文件索引 + 本地复用。
//! 索引文件 .localtrans-inbox-index.json 存于下载目录根;损坏降级空表。

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize, Clone, Debug)]
struct IndexEntry {
    path: PathBuf,
    size: u64,
}

#[derive(Serialize, Deserialize, Default, Debug)]
struct IndexFile {
    entries: HashMap<String, IndexEntry>,
}

pub struct InboxIndex {
    inner: IndexFile,
    path: PathBuf,
}

impl InboxIndex {
    /// 加载索引;文件缺失或 JSON 损坏 → 空索引(不炸,秒传退化为普通传输)
    pub fn load(index_path: &Path) -> Self {
        let inner = std::fs::read(index_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        InboxIndex { inner, path: index_path.to_path_buf() }
    }

    /// 命中条件:hash 存在且 size 与索引记录一致(防索引脏数据)
    pub fn lookup(&self, hash: &str, size: u64) -> Option<&PathBuf> {
        self.inner.entries.get(hash)
            .filter(|e| e.size == size)
            .map(|e| &e.path)
    }

    /// 命中时文件已不存在 → 删除该索引项(惰性清理)并返回未命中语义由调用方处理;
    /// 本方法只负责写入记录。
    pub fn insert(&mut self, hash: String, path: PathBuf, size: u64) {
        self.inner.entries.insert(hash, IndexEntry { path, size });
    }

    /// 移除指向已不存在文件的陈旧项(lookup 前调用)
    pub fn prune_missing(&mut self) {
        self.inner.entries.retain(|_, e| e.path.exists());
    }

    /// 原子落盘(tmp+rename);失败仅记日志不中断传输
    pub fn save(&self) {
        let tmp = self.path.with_extension("json.tmp");
        if let Ok(b) = serde_json::to_vec(&self.inner) {
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }
}

/// 把已命中的源文件放到目标位置:源==目标直接返回;先试硬链,
/// 失败回退复制;目标同名冲突按 "name (1).ext" 递增(镜像 write_small_file)。
pub fn place_dedup_copy(src: &Path, dest_dir: &Path, file_name: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dest_dir)?;
    let first = dest_dir.join(file_name);
    if same_file(src, &first) {
        return Ok(first);
    }
    // 冲突重命名:name (1).ext、name (2).ext ...
    let stem = {
        let f = Path::new(file_name);
        f.file_stem().and_then(|s| s.to_str()).unwrap_or(file_name).to_string()
    };
    let ext = Path::new(file_name)
        .extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e)).unwrap_or_default();
    let mut candidate = first.clone();
    let mut n = 1u32;
    while candidate.exists() && !same_file(src, &candidate) {
        candidate = dest_dir.join(format!("{} ({}){}", stem, n, ext));
        n += 1;
    }
    if same_file(src, &candidate) {
        return Ok(candidate);
    }
    // 硬链优先(零拷贝);跨设备/不支持 → fs::copy
    if std::fs::hard_link(src, &candidate).is_err() {
        std::fs::copy(src, &candidate)?;
    }
    Ok(candidate)
}

fn same_file(a: &Path, b: &Path) -> bool {
    if !b.exists() || !a.exists() {
        return false;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// 流式整体 SHA-256(64KiB 缓冲,大文件不占内存)
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut h = Sha256::new();
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break; }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}
```

mod.rs 加 `pub mod dedup;`。Cargo.toml 确认(已依赖 sha2/serde_json/tempfile——manifest.rs 已用 Sha256,无需新依赖)。

- [ ] **Step 3: 跑测试转绿**

`cargo test -p localtrans-core dedup -- --test-threads=1`
Expected: 6 个测试全 PASS

- [ ] **Step 4: Commit**

```bash
git add crates/localtrans-core/src/transfer/dedup.rs crates/localtrans-core/src/transfer/mod.rs
git commit -m "feat(core): dedup 收件箱索引——hash 查询/原子落盘/本地复用放置

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 8: C 发送侧——hash 计算与协议携带

**Files:**
- Modify: `crates/localtrans-core/src/protocol.rs`(OfferFile.hash、MetaResp.file_hash、OfferResp.skip)
- Modify: `crates/localtrans-core/src/transfer/engine.rs`(push_files_inner 879-890 小文件同步算、register_push_job_rel 1091、后台 hash 任务、MetaResp 构造点 1729)
- Test: protocol.rs 测试区、engine.rs 测试区

**Interfaces:**
- Consumes: Task 7 的 `sha256_file`
- Produces: `OfferFile.hash: Option<String>`(serde default)、`MetaResp.file_hash: Option<String>`(serde default)、`OfferResp.skip: Vec<String>`(serde default,小文件秒传排除表)、`get_push_hash(offer_id, key)`;Task 9 消费

- [ ] **Step 1: 写失败测试(serde 兼容)**

protocol.rs 测试区加:

```rust
    #[test]
    fn offer_file_hash_default_compat() {
        // 旧端不带 hash 字段 → None(向后兼容)
        let old = r#"{"name":"a.mp4","size":10,"rel_dir":""}"#;
        let f: OfferFile = serde_json::from_str(old).unwrap();
        assert_eq!(f.hash, None);
        let new = r#"{"name":"a.mp4","size":10,"rel_dir":"","hash":"abc"}"#;
        let f2: OfferFile = serde_json::from_str(new).unwrap();
        assert_eq!(f2.hash.as_deref(), Some("abc"));
    }

    #[test]
    fn meta_resp_and_offer_resp_defaults_compat() {
        let old_mr = r#"{"job_id":1,"file_name":"a","total_size":1,"chunk_hashes":[]}"#;
        let mr: ControlMsg = serde_json::from_str(
            &format!(r#"{{"type":"meta_resp",...}}"#) // 按 tag 格式改写,见下
        ).unwrap_or(ControlMsg::MetaResp {
            job_id: 1, file_name: "a".into(), total_size: 1,
            chunk_hashes: vec![], file_hash: None,
        });
        let _ = old_mr; // 实现者按既有 roundtrip 测试的直接构造法改写,
                        // 核心:新字段全 #[serde(default)] 且旧 JSON 能解
        let resp = ControlMsg::OfferResp { accepted: true, save_dir: None, reason: None, skip: vec![] };
        let buf = encode_control(&resp);
        assert!(!String::from_utf8_lossy(&buf[4..]).contains("skip")); // 空时省略
    }
```

(第二个测试的 JSON 拼接按 protocol.rs 既有 roundtrip 测试风格落地——直接构造结构体 + encode/decode 对比即可,不要留注释式伪码。)

Run:`cargo test -p localtrans-core offer_file_hash -- --test-threads=1`
Expected: FAIL(字段不存在)

- [ ] **Step 2: 加协议字段**

protocol.rs:

```rust
/// File offered for transfer
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OfferFile {
    pub name: String,
    pub size: u64,
    pub rel_dir: String,
    /// v0.10.0 整体 SHA-256(小文件 offer 时即带;大文件后台算、经 MetaResp 补)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}
```

MetaResp 加:

```rust
    MetaResp {
        job_id: u64,
        file_name: String,
        total_size: u64,
        chunk_hashes: Vec<String>,
        /// v0.10.0 大文件整体 hash(发送方后台算完存注册表,此处回填)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_hash: Option<String>,
    },
```

OfferResp 加:

```rust
    OfferResp {
        accepted: bool,
        save_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<OfferDenyReason>,
        /// v0.10.0 小文件秒传排除表(接收方已持有的文件 hash;发送方批流跳过)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skip: Vec<String>,
    },
```

**全 crate 编译修**:OfferFile/MetaResp/OfferResp 的所有构造点(FFI/PC/core 测试)按编译错误逐个补 `hash: None` / `file_hash: None` / `skip: vec![]`。

- [ ] **Step 3: 发送侧计算与注册表**

engine.rs:

1. `push_hashes` 进程级注册表(仿 push_controls 1086-1089):

```rust
/// v0.10.0 推送任务的大文件 hash 槽:key = "rel_dir/name"(与 resolve_push_file
/// 的 req_path 组合规则一致);后台算完填入,MetaReq 路由回填 MetaResp.file_hash
fn push_hashes() -> &'static std::sync::Mutex<HashMap<u64, Vec<(String, String)>>> {
    static H: std::sync::OnceLock<std::sync::Mutex<HashMap<u64, Vec<(String, String)>>>> =
        std::sync::OnceLock::new();
    H.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn set_push_hash(offer_id: u64, key: String, hash: String) {
    if let Some(v) = push_hashes().lock().unwrap().get_mut(&offer_id) {
        if let Some(slot) = v.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = hash;
        }
    }
}

fn get_push_hash(offer_id: u64, key: &str) -> Option<String> {
    push_hashes().lock().unwrap()
        .get(&offer_id)?
        .iter().find(|(k, _)| k == key)
        .map(|(_, h)| h.clone())
}
```

`remove_push_job`(1100-1103)同时清 hash 槽:`push_hashes().lock().unwrap().remove(&offer_id);`

2. `push_files_inner` 构造 offer_files 的循环(879-890)加同步小文件 hash:

```rust
        let hash = if metadata.len() <= SMALL_FILE_LIMIT {
            Some(crate::transfer::dedup::sha256_file(path)?)
        } else {
            None // 大文件后台算,经 MetaResp 携带
        };
        offer_files.push(crate::protocol::OfferFile {
            name: file_name.to_string(),
            size: metadata.len(),
            rel_dir: rel_dir.clone(),
            hash,
        });
```

3. register_push_job_rel 之后(913-915 has_large 块内)启动后台 hash 任务:

```rust
    if has_large {
        register_push_job_rel(job_id, large_files.iter().map(|(p, o)| (p.clone(), o.rel_dir.clone())).collect());
        // v0.10.0:后台并行算大文件整体 hash——不阻塞 offer 发送;
        // 接收方 accept 后反向 MetaReq 时通常已就绪(1GB ≈ 3-5s SSD)
        let hash_files: Vec<(String, PathBuf)> = large_files.iter().map(|(p, o)| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let key = if o.rel_dir.is_empty() { name } else { format!("{}/{}", o.rel_dir.trim_matches('/'), name) };
            (key, p.clone())
        }).collect();
        tokio::spawn(async move {
            for (key, path) in hash_files {
                match tokio::task::spawn_blocking(move || crate::transfer::dedup::sha256_file(&path)).await {
                    Ok(Ok(h)) => set_push_hash(job_id, key, h),
                    _ => {} // 算失败→槽位留空→本次不秒传,正常传输
                }
            }
        });
        // 预填空槽(MetaReq 先到时 get 返回 None 而非 miss)
        let keys: Vec<String> = ...同上 key 集合;
        push_hashes().lock().unwrap().insert(job_id, keys.into_iter().map(|k| (k, String::new())).collect());
    }
```

(实现时把 keys 集合先算出来复用,别写两遍循环;空串槽 = 未就绪,get_push_hash 命中空串时返回 None:`.filter(|(_, h)| !h.is_empty())`——把 filter 加进 get_push_hash。)

4. MetaReq 路由 MetaResp 构造点(1729-1734)回填:

```rust
                    // v0.10.0:push 前缀任务回填大文件整体 hash(后台已算完或留 None)
                    let file_hash = share_id.strip_prefix("push:")
                        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                        .and_then(|oid| get_push_hash(oid, &path));
                    let resp = ControlMsg::MetaResp {
                        job_id,
                        file_name: m.file_name.clone(),
                        total_size: m.total_size,
                        chunk_hashes: m.chunk_hashes.clone(),
                        file_hash,
                    };
```

- [ ] **Step 4: 编译 + 回归**

```bash
cargo test -p localtrans-core -- --test-threads=1
cargo test -p localtrans-ffi -- --test-threads=1
```

Expected: 编译过;回归无新增失败(OfferFile 等构造点已在 Step 2 修补)

- [ ] **Step 5: Commit**

```bash
git add crates/localtrans-core/src/protocol.rs crates/localtrans-core/src/transfer/engine.rs
git commit -m "feat(core): 秒传发送侧——hash 计算/OfferFile.hash/MetaResp.file_hash 携带

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 9: C 接收侧——秒传判定 + InstantHit + 索引进账 + E2E

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs`(ProgressEvent 加 InstantHit 366、offer 编排 1855-1998、recv_push_large_file 1184 后、send_small_files_batched 调用侧 960-979 过滤)
- Test: engine.rs 测试区

**Interfaces:**
- Consumes: Task 7 全部 + Task 8 的 hash 字段/get_push_hash
- Produces: `ProgressEvent::InstantHit { job_id, name, total }`(Task 10 双壳消费);接收侧行为契约

- [ ] **Step 1: 写失败 E2E(大文件二次推送零块传输)**

engine.rs 测试区(2980 行 E2E 旁)加,搭建照抄既有两设备模式:

```rust
    #[tokio::test]
    async fn push_same_large_file_twice_second_is_instant() {
        // 搭建:互信两设备;src = 5MiB 随机内容文件;乙收集 inbound_recv_hook 事件
        // 第一次推送:等待完成,断言乙侧目标文件内容一致
        // 第二次推送同一文件:
        //   断言乙收到 InstantHit{job_id, ..}(名字/大小匹配)
        //   断言乙侧新目标文件内容与 src 一致
        //   断言甲侧第二次推送期间无 SourceChunkDone 事件(零块服务)
        //   断言第二次 push_files_rel 返回 Ok
    }
```

(内部实现按既有 E2E 的 helper 落地,断言四条如上;文件大小 >1MiB 走大文件路径。)

Run:`cargo test -p localtrans-core push_same_large_file_twice -- --test-threads=1`
Expected: FAIL(无 InstantHit 事件)

- [ ] **Step 2: ProgressEvent::InstantHit + 接收侧大文件判定**

engine.rs ProgressEvent 枚举(396 SourceFailed 后)加:

```rust
    /// v0.10.0 秒传命中:接收方本地复用完成,零块传输(壳层置 done+instant)
    InstantHit { job_id: u64, name: String, total: u64 },
```

recv_push_large_file:total_size 校验后(1195)、manifest 构造(1197)前插入:

```rust
    // v0.10.0 秒传判定:MetaResp 带整体 hash 且收件箱索引命中(大小一致、文件在)
    // → 本地复用,零块传输
    if let Some(hash) = file_hash.as_ref().filter(|h| !h.is_empty()) {
        let index_path = parts_root.join(".localtrans-inbox-index.json");
        let mut index = crate::transfer::dedup::InboxIndex::load(&index_path);
        index.prune_missing();
        if let Some(src) = index.lookup(hash, total_size) {
            let src = src.clone();
            let dest = crate::transfer::dedup::place_dedup_copy(&src, &file_dest, &file_name)?;
            index.insert(hash.clone(), dest.clone(), total_size);
            index.save();
            tracing::info!("秒传命中: {} → 已复用(hash {}…)", total_size, &hash[..8.min(hash.len())]);
            if let Err(e) = sm.send_ctrl(peer, ControlMsg::RecvAck { job_id }).await {
                tracing::warn!("秒传 RecvAck 失败: {}", e);
            }
            let _ = progress.send(ProgressEvent::InstantHit {
                job_id, name: file_name.clone(), total: total_size,
            }).await;
            return Ok(());
        }
    }
```

(MetaResp 解构处 1173/1182 同步加 `file_hash` 字段;正常 finalize 后(1222 tracing 后)索引进账:)

```rust
    // v0.10.0 正常传输完成 → 索引进账(下次同文件秒传)
    if let Some(hash) = file_hash.as_ref().filter(|h| !h.is_empty()) {
        let index_path = parts_root.join(".localtrans-inbox-index.json");
        let mut index = crate::transfer::dedup::InboxIndex::load(&index_path);
        index.insert(hash.clone(), final_path.clone(), total_size);
        index.save();
    }
```

- [ ] **Step 3: 小文件 accept 时判定 + OfferResp.skip + 发送方过滤**

接收方 offer 编排(1855-1918):OfferResp 构造前算 skip(download_dir 解析提前):

```rust
                    // v0.10.0 小文件秒传:accept 前查收件箱索引,已持有的回 skip 表
                    let default_download_dir = ctx.config.read().await.download_dir.clone();
                    let download_dir = if let Some(save_dir_str) = save_dir {
                        PathBuf::from(save_dir_str)
                    } else {
                        default_download_dir
                    };
                    let inbox_index_path = download_dir.join(".localtrans-inbox-index.json");
                    let mut inbox_index = crate::transfer::dedup::InboxIndex::load(&inbox_index_path);
                    inbox_index.prune_missing();
                    let skip: Vec<String> = files.iter()
                        .filter(|f| f.size <= crate::transfer::engine::SMALL_FILE_LIMIT)
                        .filter_map(|f| f.hash.as_ref().and_then(|h|
                            inbox_index.lookup(h, f.size).map(|_| h.clone())
                        ))
                        .collect();
```

OfferResp(1889)加 `skip: skip.clone(),`;下方原 `default_download_dir`/`download_dir` 定义(1913-1918)删掉(已提前)。

accept 编排 spawn 内(1960 前)小文件本地复用 + 从批流列表剔除:

```rust
                        let mut small: Vec<crate::protocol::OfferFile> = files
                            .iter()
                            .filter(|f| f.size <= SMALL_FILE_LIMIT && !skip.contains(f.hash.as_ref().unwrap_or(&String::new())))
                            .cloned()
                            .collect();
                        // v0.10.0 秒传小文件:本地复用 + InstantHit(不进批流)
                        let small_instant: Vec<&crate::protocol::OfferFile> = files.iter()
                            .filter(|f| f.size <= SMALL_FILE_LIMIT
                                && f.hash.as_ref().map(|h| skip.contains(h)).unwrap_or(false))
                            .collect();
                        for f in &small_instant {
                            if let (Some(h), Some(src)) = (f.hash.as_ref(),
                                    f.hash.as_ref().and_then(|h| inbox_index.lookup(h, f.size)).cloned()) {
                                let mut dest_dir = download_dir.clone();
                                for comp in f.rel_dir.split('/').filter(|s| !s.is_empty()) {
                                    dest_dir.push(sanitize_component(comp));
                                }
                                if let Ok(dest) = crate::transfer::dedup::place_dedup_copy(&src, &dest_dir, &f.name) {
                                    inbox_index.insert(h.clone(), dest, f.size);
                                    let _ = pump_tx.send(ProgressEvent::InstantHit {
                                        job_id: offer_id, name: f.name.clone(), total: f.size,
                                    }).await;
                                }
                            }
                        }
                        inbox_index.save();
```

(inbox_index 需 move 进 spawn——在外层 `let inbox_index = ...` 后 clone 语义:InboxIndex 未实现 Clone,改为 spawn 前重新 `InboxIndex::load` 一次移入。)

发送方 push_files_inner:accepted 后(960 `let has_small` 前)过滤:

```rust
    // v0.10.0 小文件秒传:接收方回 skip 表 → 批流剔除(带宽零消耗)
    if !resp_skip.is_empty() {
        small_files.retain(|(_, offer)| {
            offer.hash.as_ref().map(|h| !resp_skip.contains(h)).unwrap_or(true)
        });
    }
```

(OfferResp 解构 942 处加 `skip` 到元组;`large_files` 不过滤——接收方自判定,注册表项无人取即被 remove_push_job 清理。)

- [ ] **Step 4: 小文件秒传 E2E + 全量回归**

补一个小文件版 E2E(同 Step 1 模式,文件 ≤1MiB):二次推送断言 InstantHit + 内容一致 + 乙侧 recv_small_files_batched 的 Started 事件不再出现该文件。

```bash
cargo test -p localtrans-core push_same_large_file_twice -- --test-threads=1
cargo test -p localtrans-core push_same_small_file -- --test-threads=1
cargo test -p localtrans-core -- --test-threads=1
```

Expected: 两个新 E2E PASS;全量回归无新增失败

- [ ] **Step 5: Commit**

```bash
git add crates/localtrans-core/src/transfer/engine.rs
git commit -m "feat(core): 接收侧秒传——索引命中本地复用/InstantHit/skip 表/完成进账

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 10: 双壳 InstantHit 状态分支

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs`(handle_recv_progress_event 加 InstantHit 分支)
- Modify: `src-tauri/src/main.rs`(PE::InstantHit 分支,950-981 recv 泵内)

**Interfaces:**
- Consumes: Task 9 的 `ProgressEvent::InstantHit`
- Produces: 行终态 `state="done", instant=true, done=total`(Kotlin Task 12 消费徽标)

- [ ] **Step 1: FFI 失败测试**

lib.rs 测试模块:

```rust
    #[tokio::test]
    async fn instant_hit_marks_row_done_instant() {
        let (st, cb) = test_app_state();
        handle_recv_progress_event(&st, &cb, localtrans_core::transfer::ProgressEvent::InstantHit {
            job_id: 9, name: "dup.bin".into(), total: 555,
        }).await;
        let dto = st.transfer_get_mut(9).await.unwrap();
        assert_eq!(dto.state, "done");
        assert!(dto.instant);
        assert_eq!(dto.done, 555);
        // 事件面:TransferUpdated + TransferDone(ok)
        let events = cb.events.lock().unwrap();
        assert!(events.iter().any(|e| matches!(e, AppEvent::TransferDone { ok: true, .. })));
    }
```

(SharedCb 的字段名/取事件方式照抄既有测试。)

Run:`cargo test -p localtrans-ffi instant_hit_marks_row -- --test-threads=1`
Expected: FAIL(InstantHit 无分支,行不存在)

- [ ] **Step 2: FFI 实现**

handle_recv_progress_event(匹配分支区)加:

```rust
        localtrans_core::transfer::ProgressEvent::InstantHit { job_id, name, total } => {
            // v0.10.0 秒传:直接落终态(行缺失也建——接收侧行可能尚未创建)
            let mut dto = st.transfer_get_mut(job_id).await.unwrap_or_else(|| crate::dto::TransferDto {
                job_id, name: name.clone(), total, done: 0,
                state: "active".to_string(), speed_bps: 0, peer: String::new(),
                direction: "rx".to_string(), local_role: "receiver".to_string(),
                progress_percent: 0, eta_secs: -1, fail_reason: String::new(),
                local_path: None, remote_done: 0, instant: false,
            });
            dto.name = name; dto.total = total; dto.done = total;
            dto.state = "done".to_string();
            dto.instant = true;
            dto.progress_percent = 100;
            st.transfer_update(job_id, dto.clone()).await;
            cb.on_event(AppEvent::TransferUpdated { transfer: dto });
            cb.on_event(AppEvent::TransferDone { job_id, ok: true, fail_reason: String::new() });
        }
```

Run Step 1,Expected: PASS

- [ ] **Step 3: PC 壳分支**

main.rs recv 泵(950-981 匹配区)加:

```rust
                                PE::InstantHit { job_id, name, total } => {
                                    if let Some(mut dto) = st.transfer_get_mut(job_id).await {
                                        dto.name = name; dto.total = total; dto.done = total;
                                        dto.state = "done".into();
                                        dto.instant = true;
                                        dto.finished_at_ms = Some(now_ms());
                                        st.transfer_update(job_id, dto).await;
                                    }
                                }
```

- [ ] **Step 4: 回归 + Commit**

```bash
cargo test -p localtrans-ffi -- --test-threads=1
cargo build -p localtrans --release 2>&1 | tail -3
git add crates/localtrans-ffi/src/lib.rs src-tauri/src/main.rs
git commit -m "feat(shell): InstantHit 双壳分支——秒传行落 done+instant 终态

Co-Authored-By: Claude <noreply@anthropic.com>"
```

Expected: 除已知端口占用外无新增失败;PC 编译过

---

### Task 11: Kotlin 绑定重生成 + AppException 手工补丁

**Files:**
- Modify: `android/app/src/main/java/uniffi/localtrans_ffi/localtrans_ffi.kt`(重生成 + 补丁)

**Interfaces:**
- Consumes: Task 5/10 的 FFI 全部改动(so 已含 remote_done/instant)
- Produces: 绑定含 `TransferDto.remoteDone/instant`——Task 12 消费

- [ ] **Step 1: 重编 so**

```bash
export ANDROID_NDK_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/android-sdk/ndk/27.0.12077973" && export CARGO_HOME="$HOME/.cargo" && export PATH="$CARGO_HOME/bin:$PATH" && cargo ndk -t arm64-v8a -t x86_64 -o android/app/src/main/jniLibs build -p localtrans-ffi --release
```

Expected: 编译完成无错

- [ ] **Step 2: 生成绑定到临时目录并 diff 校验**

```bash
cargo run -p localtrans-ffi --bin uniffi-bindgen -- generate --library target/aarch64-linux-android/release/liblocaltrans_ffi.so --language kotlin --out-dir /tmp/uniffi_check && diff /tmp/uniffi_check/uniffi/localtrans_ffi/localtrans_ffi.kt android/app/src/main/java/uniffi/localtrans_ffi/localtrans_ffi.kt
```

Expected: diff 只剩 AppException 补丁形态(errorMessage/message override 约 20 行)——若出现其他差异,把生成文件覆盖过去再重做补丁:

```kotlin
    // AppException 补丁(字段名与 Throwable.message 冲突的处理):
    val errorMessage: kotlin.String = `message`,
    override val message: kotlin.String get() = errorMessage
```

- [ ] **Step 3: 编译验证**

```bash
cd android && export JAVA_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8" && export PATH="C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin:$PATH" && gradle.bat :app:compileDebugKotlin
```

Expected: BUILD SUCCESSFUL

- [ ] **Step 4: Commit**

```bash
git add android/app/src/main/java/uniffi/localtrans_ffi/localtrans_ffi.kt android/app/src/main/jniLibs
git commit -m "chore(android): 重生成 uniFFI 绑定——remote_done/instant 字段+AppException 补丁

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 12: Kotlin UI——双进度条 + 秒传徽标

**Files:**
- Modify: `android/app/src/main/java/com/localtrans/app/ui/transfers/TransfersViewModel.kt`(TransferUi 模型/toUiModel)
- Modify: `android/app/src/main/java/com/localtrans/app/ui/transfers/TransfersScreen.kt`(TransferCard 双进度条、StatusBadge 秒传)
- Test: `android/app/src/test/java/com/localtrans/app/ui/transfers/TransfersViewModelTest.kt`

**Interfaces:**
- Consumes: Task 11 绑定的 `TransferDto.remoteDone: ULong`、`TransferDto.instant: Boolean`
- Produces: 无(终端 UI 任务)

- [ ] **Step 1: 写失败测试**

TransfersViewModelTest.kt 加:

```kotlin
    @Test
    fun `transfer ui exposes remote done and instant`() = runTest {
        fakeRepo.emitEvent(AppEvent.TransferUpdated(
            transfer = TransferDto(
                jobId = 88u, name = "big.mp4", total = 1000u, done = 700u,
                state = "active", speedBps = 0u, peer = "PC", direction = "push",
                localRole = "source-push", progressPercent = 70u, etaSecs = -1,
                failReason = "", localPath = null,
                remoteDone = 500u, instant = false
            )
        ))
        testDispatcher.scheduler.advanceUntilIdle()
        val t = viewModel.transfers.value.first { it.jobId == 88L }
        assertEquals(500L, t.remoteDone)

        fakeRepo.emitEvent(AppEvent.TransferUpdated(
            transfer = TransferDto(
                jobId = 89u, name = "dup.bin", total = 10u, done = 10u,
                state = "done", speedBps = 0u, peer = "PC", direction = "rx",
                localRole = "receiver", progressPercent = 100u, etaSecs = -1,
                failReason = "", localPath = null,
                remoteDone = 0u, instant = true
            )
        ))
        testDispatcher.scheduler.advanceUntilIdle()
        assertTrue(viewModel.transfers.value.first { it.jobId == 89L }.instant)
    }
```

(TransferUi 构造处若在别文件,同步补默认参数。)

Run:`gradle.bat :app:testDebugUnitTest --tests "*TransfersViewModelTest*"`
Expected: FAIL(remoteDone/instant 无字段)

- [ ] **Step 2: TransferUi 模型扩展**

TransfersViewModel.kt 的 TransferUi(文件内或同包模型文件)加字段 `val remoteDone: Long = 0`、`val instant: Boolean = false`;toUiModel(260-276)加:

```kotlin
        remoteDone = remoteDone.toLong(),
        instant = instant
```

(FakeTransfersRepo 的 TransferDto 构造如有,补新参数——uniffi Record 构造全参必填。)

Run Step 1,Expected: PASS

- [ ] **Step 3: TransferCard 双进度条 + StatusBadge 秒传**

TransfersScreen.kt 进度条区(209-219)改为:

```kotlin
            // Progress bar
            if (transfer.state == "pending") {
                LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
            } else {
                LinearProgressIndicator(
                    progress = { transfer.progressPercent / 100f },
                    modifier = Modifier.fillMaxWidth()
                )
            }
            // v0.10.0 双进度:push 方向且对端已上报(remoteDone>0)时显示"对方已收"第二条
            if (transfer.direction == "push" && transfer.remoteDone > 0
                && (transfer.state == "active" || transfer.state == "paused")
                && transfer.remoteDone < transfer.total) {
                Spacer(modifier = Modifier.height(4.dp))
                LinearProgressIndicator(
                    progress = { (transfer.remoteDone.toFloat() / transfer.total).coerceIn(0f, 1f) },
                    modifier = Modifier.fillMaxWidth()
                )
                Text(
                    text = "对方已收 ${Formatters.formatFileSize(transfer.remoteDone.toULong())}",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant
                )
            }
```

StatusBadge 调用处(204 行)改为带秒传判定:

```kotlin
                StatusBadge(
                    if (transfer.state == "done" && transfer.instant) "instant" else transfer.state
                )
```

StatusBadge 的 when(394-402)加:

```kotlin
        "instant" -> MaterialTheme.colorScheme.tertiary to "秒传"
```

(注:state 仍为 "done"——hasFinished/清空记录/FFI 持久化终态判断零改动,仅显示层映射。)

- [ ] **Step 4: 编译 + 全部单测 + Commit**

```bash
gradle.bat :app:compileDebugKotlin :app:testDebugUnitTest
git add android/app/src/main/java/com/localtrans/app/ui/transfers/ android/app/src/test/java/com/localtrans/app/ui/transfers/
git commit -m "feat(android): 传输卡片双进度条(已发送/对方已收)+秒传徽标

Co-Authored-By: Claude <noreply@anthropic.com>"
```

Expected: BUILD SUCCESSFUL

---

### Task 13: v0.10.0 收尾——版本/CHANGELOG/打包/装机

**Files:**
- Modify: `Cargo.toml`(workspace version)、`src-tauri/tauri.conf.json`、`android/app/build.gradle.kts`(versionName)、`CHANGELOG.md`

**Interfaces:**
- Consumes: Task 1-12 全部
- Produces: v0.10.0 发布产物

- [ ] **Step 1: 版本三处对齐 0.10.0 + CHANGELOG**

Cargo.toml `[workspace.package]` version = "0.10.0";tauri.conf.json `"version": "0.10.0"`;build.gradle.kts `versionName = "0.10.0"`。CHANGELOG.md 顶部加:

```markdown
## [0.10.0] - 2026-08-25

### 新增
- Android 文件浏览长按菜单(重命名/删除/选择多项;远程页含下载到本机)
- 远程页单击文件直接拉取(一步入口,Snackbar 可跳传输页)
- 大文件推送双进度条:已发送/对方已收(RecvProgress 逐窗确认,旧对端自动降级隐藏)
- 收件箱 hash 秒传:同文件重复推送零字节传输(OfferFile.hash/MetaResp.file_hash/OfferResp.skip,新旧对端互通)
```

- [ ] **Step 2: 全量回归**

```bash
cargo test -p localtrans-core -- --test-threads=1 2>&1 | tail -3
cargo test -p localtrans-ffi -- --test-threads=1 2>&1 | tail -3
cd android && export JAVA_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8" && export PATH="C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin:$PATH" && gradle.bat :app:testDebugUnitTest 2>&1 | tail -3
```

Expected: core 全绿;FFI 除已知 7 端口占用外全绿;Kotlin 全绿

- [ ] **Step 3: 三端打包**

```bash
# PC(参照 v0.9.2 流程:cargo build --release -p localtrans + tauri 打包 + dist 组装)
# Android:gradle.bat :app:assembleRelease
# dist 目录:localtrans-v0.10.0/(zip 不含 data/)、localtrans-android-v0.10.0/(APK)
```

(具体打包脚本照 v0.9.2 的 dist 组装步骤——dist/localtrans-v0.9.2 的目录结构为模板。)

- [ ] **Step 4: 模拟器装机验证**

```bash
adb install -r android/app/build/outputs/apk/release/app-release.apk
adb shell am start -n com.localtrans.app/.MainActivity && adb logcat -d | grep -i "localtrans" | tail -5
```

Expected: "App started successfully";手动冒烟:长按菜单弹出/单击远程文件触发下载/推送大文件见双进度/同文件二推显示秒传

- [ ] **Step 5: Commit + tag**

```bash
git add Cargo.toml src-tauri/tauri.conf.json android/app/build.gradle.kts CHANGELOG.md
git commit -m "chore(release): v0.10.0——UX 优化四项收尾

Co-Authored-By: Claude <noreply@anthropic.com>"
git tag v0.10.0
```

---

## Self-Review 记录

1. **Spec 覆盖**:①菜单=Task 1/2;②双进度=Task 3/4/5/6/11/12;③单击直拉=Task 2;④秒传=Task 7/8/9/10/11/12;收尾=Task 13。spec"明确不做"清单(全盘索引/PC 双进度 UI/后台扫描/mmap 优化)无对应任务——正确。
2. **占位符扫描**:Task 8 Step 1 第二个测试有"按既有 roundtrip 风格落地"的指示性描述(已给核心断言与构造形式);Task 9 Step 1/Task 13 Step 3 引用"照抄既有 E2E/打包模板"——这两处是让实现者复用仓库既有 helper 的指路,非缺代码(关键断言已逐条列出)。
3. **类型一致性**:`RecvProgress { job_id: u64, cumulative_bytes: u64 }` 三处一致;`SourceSpeed.remote_done: u64` 定义(Task 3)与消费(Task 5 FFI/Task 6 PC)一致;`TransferDto.remote_done: u64 + instant: bool` 在 Task 5 定义、Task 10/12 消费一致;`get_push_hash(offer_id: u64, key: &str) -> Option<String>` Task 8 定义/消费一致;`InboxIndex::lookup(&self, hash: &str, size: u64) -> Option<&PathBuf>` Task 7 定义、Task 9 消费一致。
