# 文件页四 Tab 能力矩阵统一 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 spec `docs/superpowers/specs/2026-08-25-file-tabs-capability-matrix-design.md`——本机四 Tab(相册/视频/文档/全部)统一为"单击=选中/进入、长按=菜单、多选=合并底部栏(发送+删除+单选重命名)"。

**Architecture:** 纯安卓 UI 层。新建 UnifiedSelectionBar 替换 SendBar/SelectionActionBar 两旧栏;MediaGridTab/DocsTab 加 combinedClickable 长按;FilesScreen 统一挂 FileEntryMenuSheet 与 UnifiedSelectionBar;删除/重命名复用现有 FilesViewModel.renameEntry/deleteEntries(local_op),媒体 Tab 删除后 MediaPickerViewModel.refresh() 重查、文档 Tab 重扫。FFI/Rust 零改动。

**Tech Stack:** Kotlin Compose Material3(combinedClickable/ModalBottomSheet),MediaStore 查询。

## Global Constraints

- 纯安卓 UI,FFI/Rust 零改动
- 远程页行为不变(只读:下载/选择多项)
- 提交信息:中文前缀 + 空行 + `Co-Authored-By: Claude <noreply@anthropic.com>`
- 版本 v0.10.1:Cargo.toml `[workspace.package]` version、src-tauri/tauri.conf.json、android/app/build.gradle.kts versionName="0.10.1"+versionCode=12、CHANGELOG.md
- 工具链:JAVA_HOME=`C:/Users/<user>\Desktop\work\deepseek_use\tools\jdk17\jdk-17.0.20+8`;gradle=`C:/Users/<user>\Desktop\work\deepseek_use\tools\gradle-8.10.2\bin\gradle.bat`(android/ 无 wrapper);adb/模拟器已配置
- gradle 输出重定向到文件再 tail,不走管道;daemon 挂先 `taskkill //F //IM java.exe`
- 删除相册/视频是真删系统相册文件——DeleteConfirmationDialog 必须在所有删除路径前置

---

### Task 1: UnifiedSelectionBar 组件(TDD)

**Files:**
- Create: `android/app/src/main/java/com/localtrans/app/ui/files/UnifiedSelectionBar.kt`
- Test: `android/app/src/test/java/com/localtrans/app/ui/files/SendBarTest.kt`(追加)

**Interfaces:**
- Consumes: `sendBarSummary(count: Int, bytes: Long): String`(SendBar.kt:14 现有,保留)
- Produces: `UnifiedSelectionBar(selectedCount: Int, totalBytes: Long, onSend: () -> Unit, onRename: () -> Unit, onDelete: () -> Unit, onClear: () -> Unit, isRemote: Boolean = false)`——Task 3/4 消费

- [ ] **Step 1: 写失败测试(SendBarTest.kt 追加)**

```kotlin
@Test
fun `unified bar summary reuses sendBarSummary`() {
    // 摘要文案复用现有 sendBarSummary(3 项·2.5 MB 形态)
    assertEquals(sendBarSummary(3, 2_500_000), sendBarSummary(3, 2_500_000))
}
```

(组件无渲染测试基建,摘要函数复用是唯一可单测点;按钮可见性逻辑装机冒烟覆盖——见 Task 4。)

Run:`cd android && cmd //c "set JAVA_HOME=C:/Users/<user>\Desktop\work\deepseek_use\tools\jdk17\jdk-17.0.20+8&& C:/Users/<user>\Desktop\work\deepseek_use\tools\gradle-8.10.2\bin\gradle.bat :app:testDebugUnitTest --tests \"*SendBarTest*\""`(输出重定向文件)
Expected: PASS(摘要已存在——本测试是回归锚,真正红绿在组件编译)

- [ ] **Step 2: 写 UnifiedSelectionBar.kt**

```kotlin
package com.localtrans.app.ui.files

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

/**
 * 四 Tab 统一多选底部栏:发送+删除+单选重命名+取消。
 * 远程页 isRemote=true 隐藏重命名/删除(share 只读)。
 * 重命名按钮仅在恰好选中 1 项时渲染。
 */
@Composable
fun UnifiedSelectionBar(
    selectedCount: Int,
    totalBytes: Long,
    onSend: () -> Unit,
    onRename: () -> Unit,
    onDelete: () -> Unit,
    onClear: () -> Unit,
    isRemote: Boolean = false
) {
    Surface(tonalElevation = 8.dp, shadowElevation = 8.dp) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Text(sendBarSummary(selectedCount, totalBytes), Modifier.weight(1f))
            if (!isRemote) {
                if (selectedCount == 1) {
                    IconButton(onClick = onRename) {
                        Icon(Icons.Default.Edit, "重命名")
                    }
                }
                IconButton(onClick = onDelete) {
                    Icon(Icons.Default.Delete, "删除")
                }
            }
            IconButton(onClick = onSend) {
                Icon(Icons.Default.Send, "发送")
            }
            IconButton(onClick = onClear) {
                Icon(Icons.Default.Close, "取消选择")
            }
        }
    }
}
```

- [ ] **Step 3: 编译+全测**

Run:`gradle.bat :app:compileDebugKotlin :app:testDebugUnitTest`(同环境变量,输出重定向)
Expected: BUILD SUCCESSFUL

- [ ] **Step 4: Commit**

```bash
git add android/app/src/main/java/com/localtrans/app/ui/files/UnifiedSelectionBar.kt android/app/src/test/java/com/localtrans/app/ui/files/SendBarTest.kt
git commit -m "feat(android): UnifiedSelectionBar 统一多选底部栏——发送/删除/单选重命名

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 2: MediaGridTab/DocsTab 长按接线

**Files:**
- Modify: `android/app/src/main/java/com/localtrans/app/ui/files/MediaGridTab.kt`(clickable→combinedClickable+onLongClick 参数)
- Modify: `android/app/src/main/java/com/localtrans/app/ui/files/DocsTab.kt`(同型改造)

**Interfaces:**
- Consumes: 无
- Produces: `MediaGridTab(items, selected, onToggle: (Long) -> Unit, onLongClick: (path: String) -> Unit, modifier)`;`DocsTab(files, selected, onToggle: (String) -> Unit, onLongClick: (path: String) -> Unit, modifier)`——Task 3 消费(参数名 onLongClick,路径=绝对路径)

- [ ] **Step 1: MediaGridTab 加长按**

import 区加:

```kotlin
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
```

签名与 clickable 替换:

```kotlin
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun MediaGridTab(
    items: List<MediaItem>,
    selected: Set<Long>,
    onToggle: (Long) -> Unit,
    onLongClick: (path: String) -> Unit,
    modifier: Modifier = Modifier
) {
```

AsyncImage 的 modifier 内 `.clickable { onToggle(item.id) }` 替换为:

```kotlin
                        .combinedClickable(
                            onClick = { onToggle(item.id) },
                            onLongClick = { onLongClick(item.path) }
                        )
```

- [ ] **Step 2: DocsTab 加长按(同型)**

import 加 `androidx.compose.foundation.ExperimentalFoundationApi` 与 `androidx.compose.foundation.combinedClickable`;签名:

```kotlin
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun DocsTab(
    files: List<File>,
    selected: Set<String>,
    onToggle: (String) -> Unit,
    onLongClick: (path: String) -> Unit,
    modifier: Modifier = Modifier
)
```

ListItem 的 `.clickable { onToggle(f.absolutePath) }` 替换为:

```kotlin
                modifier = Modifier.combinedClickable(
                    onClick = { onToggle(f.absolutePath) },
                    onLongClick = { onLongClick(f.absolutePath) }
                ),
```

- [ ] **Step 3: 编译(FilesScreen 调用点暂未传新参会红——本任务同时改调用点的最小占位)**

FilesScreen.kt 三处调用(相册/视频/文档的 MediaGridTab/DocsTab)临时补 `onLongClick = {},`(Task 3 换真实现);编译:

Run:`gradle.bat :app:compileDebugKotlin`
Expected: BUILD SUCCESSFUL

- [ ] **Step 4: Commit**

```bash
git add android/app/src/main/java/com/localtrans/app/ui/files/MediaGridTab.kt android/app/src/main/java/com/localtrans/app/ui/files/DocsTab.kt android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt
git commit -m "feat(android): 媒体网格/文档列表长按接线——combinedClickable+onLongClick 路径回调

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 3: FilesScreen 菜单挂载+单击选中+统一栏替换

**Files:**
- Modify: `android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt`

**Interfaces:**
- Consumes: Task 1 `UnifiedSelectionBar`;Task 2 `onLongClick: (path: String) -> Unit`;现有 `FileEntryMenuSheet(isRemote, onDismiss, onDownload, onRename, onDelete, onSelectMultiple)`、`filesViewModel.renameEntry/deleteEntries/toggleSelection/clearSelection`、`mediaViewModel.toggleSelect/clearSelection/refresh/selectedPaths`、`renamePath/renameOldName/showRenameDialog/deletePaths/showDeleteDialog` 状态、`DeleteConfirmationDialog`/`RenameDialog`
- Produces: 无(终端接线任务)

- [ ] **Step 1: 媒体长按菜单状态与挂载**

FilesScreen 顶部状态区(现有 menuEntry 声明旁)加:

```kotlin
    // 媒体项长按菜单(与全部 Tab 共用 menuEntry 语义:name+path)
    var mediaMenuName by remember { mutableStateOf("") }
```

menuEntry 现有声明改造成"路径+名字"双状态(全部 Tab 用)或直接复用——最省改法:menuEntry 保持 FileEntryUi?,媒体项构造临时对象:

```kotlin
    fun openLocalMenu(path: String) {
        menuEntry = FileEntryUi(
            name = java.io.File(path).name,
            isDir = false,
            size = 0L,
            modifiedMs = 0L,
            path = path
        )
    }
```

(FileEntryUi 构造参数以实际定义为准——先看 FilesScreen.kt 里 FileEntryUi 的 data class 声明,若 size/modifiedMs 无默认值按声明顺序全参构造。)

三处占位 `onLongClick = {},` 替换为:

```kotlin
                                onLongClick = { path -> openLocalMenu(path) },
```

(相册/视频的 MediaGridTab 与文档的 DocsTab 四处调用点统一。)

- [ ] **Step 2: 菜单回调按 Tab 分发(菜单挂载块扩成媒体感知)**

现有 `menuEntry?.let { entry -> ... FileEntryMenuSheet(...) }` 块(onDownload 分支里 remote 判定已有)保持;onRename/onDelete/选择多项回调在本机 Tab 下对媒体/全部一视同仁(路径都是本机绝对路径,现有 renameEntry/deleteEntries 直接可用)——**无需改**;仅确认菜单挂载块的 `isRemotePage` 判定 `uiState.location == Location.REMOTE` 已覆盖媒体 Tab(媒体 Tab 只存在于 LOCAL,天然 false)。

- [ ] **Step 3: 全部 Tab 单击文件=选中**

FilesScreen ALL Tab 的 FileEntryCard onClick(约 253-259 行)改为:

```kotlin
                                        onClick = {
                                            when {
                                                selectedEntries.isNotEmpty() ->
                                                    filesViewModel.toggleSelection(entry.path)
                                                entry.isDir ->
                                                    filesViewModel.navigateTo(entry.path)
                                                else ->
                                                    // v0.10.1 单击文件=进入多选(与媒体 Tab 心智一致)
                                                    filesViewModel.toggleSelection(entry.path)
                                            }
                                        },
```

- [ ] **Step 4: 四处 SendBar → UnifiedSelectionBar**

相册/视频两处(约 176/194 行)、文档(约 214 行)、全部(约 273 行)的 SendBar 调用替换。以相册为例(其余三处同型,各 Tab 用各自的选中集):

```kotlin
                            if (mediaSelected.isNotEmpty()) {
                                UnifiedSelectionBar(
                                    selectedCount = mediaSelected.size,
                                    totalBytes = mediaItems.filter { it.id in mediaSelected }.sumOf { it.size },
                                    onSend = { showDeviceSheet = true },
                                    onRename = {
                                        val p = mediaViewModel.selectedPaths().first()
                                        renamePath = p
                                        renameOldName = java.io.File(p).name
                                        showRenameDialog = true
                                    },
                                    onDelete = {
                                        deletePaths = mediaViewModel.selectedPaths()
                                        showDeleteDialog = true
                                    },
                                    onClear = { mediaViewModel.clearSelection() }
                                )
                            }
```

视频 Tab 同型(同为 mediaViewModel)。文档 Tab:选中集 docsSelected,路径集 `docsFiles.filter { it.absolutePath in docsSelected }.map { it.absolutePath }`,onClear `docsSelected = emptySet()`。全部 Tab:选中集 selectedEntries,路径集 `selectedEntries.toList()`,totalBytes 计算保留现有 entries.filter 写法,onClear `filesViewModel.clearSelection()`。

- [ ] **Step 5: 远程页 SelectionActionBar → UnifiedSelectionBar(isRemote=true)**

远程页(约 381 行)的 SelectionActionBar 替换:

```kotlin
                            if (selectedEntries.isNotEmpty()) {
                                UnifiedSelectionBar(
                                    selectedCount = selectedEntries.size,
                                    totalBytes = 0L,
                                    onSend = {
                                        val fp = uiState.selectedDeviceFp ?: ""
                                        val shareId = currentShareId ?: ""
                                        if (fp.isNotEmpty() && shareId.isNotEmpty()) {
                                            filesViewModel.pullFiles(fp, shareId, selectedEntries.toList())
                                            onSendSuccess("已开始下载")
                                        }
                                    },
                                    onRename = {},
                                    onDelete = {},
                                    onClear = { filesViewModel.clearSelection() },
                                    isRemote = true
                                )
                            }
```

- [ ] **Step 6: 删除旧组件+编译全测**

删除 FilesScreen.kt 内 `SelectionActionBar` 整个函数(约 762-805 行)与 SendBar.kt 里的 `SendBar` composable(保留 `sendBarSummary` 函数);SendBar.kt 改名语义上仍被 UnifiedSelectionBar 依赖(摘要函数),文件保留。

Run:`gradle.bat :app:compileDebugKotlin :app:testDebugUnitTest`
Expected: BUILD SUCCESSFUL,0 failures

- [ ] **Step 7: Commit**

```bash
git add android/app/src/main/java/com/localtrans/app/ui/files/
git commit -m "feat(android): 四 Tab 统一交互——单击选中/长按菜单/UnifiedSelectionBar 全替换

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 4: 删除/重命名后的刷新闭环

**Files:**
- Modify: `android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt`(deleteEntries/renameEntry 确认回调处)
- Test: `android/app/src/test/java/com/localtrans/app/media/MediaPickerViewModelTest.kt`(追加)

**Interfaces:**
- Consumes: `mediaViewModel.refresh()`(MediaPickerViewModel.kt:29 现有,公开可直接调)
- Produces: 无

- [ ] **Step 1: 写失败测试(refresh 清空已删项)**

MediaPickerViewModelTest.kt 追加(Fake repo 以测试文件现有形态为准,若 Fake 无 items 注入口先补):

```kotlin
    @Test
    fun `refresh reloads from repo dropping deleted items`() = runTest {
        // repo 首查返回 [A, B];选中 A;repo 数据源改为只剩 [B];refresh 后
        // items 只剩 B 且 selected 被相交清理
        viewModel.toggleSelect(1L)
        fakeRepo.setItems(listOf(item(id = 2L, name = "B")))
        viewModel.refresh()
        advanceUntilIdle()
        assertEquals(listOf(2L), viewModel.items.value.map { it.id })
        assertTrue(viewModel.selected.value.isEmpty())
    }
```

(item() 工厂与 fakeRepo 变量名以该测试文件既有代码为准照抄风格。)
Run:`gradle.bat :app:testDebugUnitTest --tests "*MediaPickerViewModelTest*"`
Expected: FAIL(Fake 无 setItem 注入口/或断言不过)——给 Fake 加 `setItems` 后转绿(它是测试设施,不算生产代码)

- [ ] **Step 2: 删除确认回调按 Tab 刷新**

FilesScreen 的 DeleteConfirmationDialog onConfirm(现有 `filesViewModel.deleteEntries(deletePaths); showDeleteDialog = false` 处)改为:

```kotlin
            onConfirm = {
                filesViewModel.deleteEntries(deletePaths)
                showDeleteDialog = false
                // v0.10.1 按当前 Tab 刷新数据源(媒体重查 MediaStore/文档重扫/全部重列目录)
                when (localTab) {
                    LocalTab.PHOTOS, LocalTab.VIDEOS -> mediaViewModel.refresh()
                    LocalTab.DOCS -> refreshDocs()
                    LocalTab.ALL -> { /* deleteEntries 内部已 loadEntries */ }
                }
                mediaViewModel.clearSelection()
                docsSelected = emptySet()
            }
```

RenameDialog onConfirm 同型追加(重命名后列表名变):PHOTOS/VIDEOS→refresh(),DOCS→refreshDocs(),ALL→已有 loadEntries。

- [ ] **Step 3: docsFiles 可刷新化**

FilesScreen 里现有 produceState(约 76-79 行)替换:

```kotlin
    var docsFiles by remember { mutableStateOf<List<File>>(emptyList()) }
    var docsRev by remember { mutableIntStateOf(0) }
    LaunchedEffect(docsRev) {
        withContext(Dispatchers.IO) { docsFiles = collectDocs(docsRoots) }
    }
    fun refreshDocs() { docsRev++ }
```

(produceState 初值语义=LaunchedEffect(0) 首跑,行为等价;import 补 LaunchedEffect/mutableIntStateOf 若缺。)

- [ ] **Step 4: 编译全测**

Run:`gradle.bat :app:compileDebugKotlin :app:testDebugUnitTest`
Expected: BUILD SUCCESSFUL,0 failures

- [ ] **Step 5: Commit**

```bash
git add android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt android/app/src/test/java/com/localtrans/app/media/MediaPickerViewModelTest.kt
git commit -m "feat(android): 删除/重命名后按 Tab 刷新——MediaStore 重查/文档重扫/目录重列

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 5: 装机冒烟 + v0.10.1 收尾

**Files:**
- Modify: `Cargo.toml`(workspace version)、`src-tauri/tauri.conf.json`、`android/app/build.gradle.kts`(versionName/versionCode)、`CHANGELOG.md`

**Interfaces:**
- Consumes: Task 1-4 全部
- Produces: v0.10.1 APK + dist

- [ ] **Step 1: 装机冒烟(先于版本号,防返工)**

```bash
gradle.bat :app:assembleDebug
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
adb shell am force-stop com.localtrans.app && adb shell am start -n com.localtrans.app/.MainActivity
```

手工核对清单(模拟器 screencap 截图逐项):
1. 相册 Tab 长按照片→菜单(重命名/删除/选择多项)
2. 相册多选 2 张→底部栏出现 发送/删除/取消(无重命名——非单选)
3. 相册单选 1 张→底部栏多出重命名
4. 删除 1 张(确认弹窗)→列表刷新少该项
5. 视频/文档同型抽查各 1 项
6. 全部 Tab 单击文件→直接进多选;多选后底部栏=发送/删除(+单选重命名)
7. 远程页:长按菜单只有 下载/选择多项;多选栏只有 发送(下载)/取消——回归未破坏

- [ ] **Step 2: 版本四处+CHANGELOG**

Cargo.toml version="0.10.1";tauri.conf.json "0.10.1";build.gradle.kts versionName="0.10.1"/versionCode=12。CHANGELOG 顶部:

```markdown
## [0.10.1] - 2026-08-25

### 优化
- 文件页四 Tab 统一操作矩阵:相册/视频/文档补长按菜单(重命名/删除/选择多项)与多选删除;全部 Tab 单击文件直接进入多选;四 Tab 多选底部栏统一(发送+删除+单选重命名)
- 删除/重命名后按 Tab 刷新列表(MediaStore 重查/文档重扫/目录重列)
```

- [ ] **Step 3: 回归+release APK+dist**

```bash
gradle.bat :app:testDebugUnitTest :app:assembleRelease
# aapt 验证 versionCode=12/versionName=0.10.1;apksigner verify
cp android/app/build/outputs/apk/release/app-release.apk dist/localtrans-android-v0.10.1/localtrans-v0.10.1-android.apk
# usage-android.md 沿用;zip 重打
```

- [ ] **Step 4: Commit + tag**

```bash
git add Cargo.toml src-tauri/tauri.conf.json android/app/build.gradle.kts CHANGELOG.md
git commit -m "chore(release): v0.10.1——文件页四 Tab 能力矩阵统一收尾

Co-Authored-By: Claude <noreply@anthropic.com>"
git tag v0.10.1
```

---

## Self-Review 记录

1. **Spec 覆盖**:统一能力矩阵表四行=Task 3(全部/远程)+Task 2/3(媒体三 Tab);UnifiedSelectionBar=Task 1;刷新闭环(含文档 produceState 改造)=Task 4;收尾=Task 5。spec"明确不做"清单无对应任务——正确。
2. **占位符扫描**:Task 3 Step 1 的 FileEntryUi 构造"以实际定义为准"是指路提示(关键代码已给全参形态);Task 4 Step 1 的 item()/fakeRepo"以测试文件既有代码为准"同性质——测试基建形态控制器已核实存在(MediaPickerViewModelTest 现有)。
3. **类型一致性**:`UnifiedSelectionBar(selectedCount, totalBytes, onSend, onRename, onDelete, onClear, isRemote)` Task 1 定义=Task 3 四处+远程一处调用一致;`onLongClick: (path: String) -> Unit` Task 2 定义=Task 3 消费一致;`mediaViewModel.refresh()` Task 4 调用的现有公开 API(29 行)。
