# 文件页四 Tab 能力矩阵统一 设计文档

日期:2026-08-25
版本目标:v0.10.1(小版本,纯安卓 UI)
前置:v0.10.0(f40a373 远程 share 只读修正后)

## 背景与问题

用户实测 v0.10.0 发现文件页四个本机 Tab 操作能力割裂:

| Tab | 现状 | 问题 |
|---|---|---|
| 相册/视频 | 单击=多选;多选后仅发送(SendBar) | 无长按菜单→无重命名/删除 |
| 文档 | 同上 | 同上 |
| 全部 | 单击文件无反应;长按菜单(重命名/删除/选择多项);多选后仅发送 | 发送入口链深(长按→选择多项→再点);单击零反馈 |

能力互补却割裂:媒体三 Tab 只能发送,全部 Tab 管理全但没有顺畅的发送入口。

## 目标能力矩阵(统一后)

| Tab | 单击 | 长按 | 多选底部栏 |
|---|---|---|---|
| 相册/视频 | 选中/取消 | 菜单:重命名/删除/选择多项 | 发送+删除(+单选时重命名)+取消 |
| 文档 | 选中/取消 | 同上 | 同上 |
| 全部 | 目录=进入;文件=选中 | 同上(现状已有) | 同上(替换现 SendBar) |
| 远程 | 目录=进入;文件=直拉 | 菜单:下载到本机/选择多项(现状) | 下载+取消(不变) |

交互语法统一:**单击=选中/进入、长按=菜单、多选后=合并底部栏**。

## 方案

方案 A(已选):统一能力矩阵+复用现有组件。零协议/FFI 改动,纯安卓 UI。
弃选 B(MediaStore URI 操作,云占位文件场景 YAGNI)/C(维持割裂只补全部 Tab)。

## 组件设计

### 1. 新组件 UnifiedSelectionBar

新建 `android/app/src/main/java/com/localtrans/app/ui/files/UnifiedSelectionBar.kt`:

```kotlin
@Composable
fun UnifiedSelectionBar(
    selectedCount: Int,
    totalBytes: Long,
    onSend: () -> Unit,
    onRename: () -> Unit,        // 仅 selectedCount == 1 时渲染重命名按钮
    onDelete: () -> Unit,
    onClear: () -> Unit,
    isRemote: Boolean = false    // true 时隐藏重命名/删除(远程只读,现状语义)
)
```

- 布局:`N 项选中 · 大小` 文本(weight 1) + 重命名(单选时) + 删除 + 发送 + 取消
- **替换并删除**旧 `SendBar`(FilesScreen.kt 内联)与 `SelectionActionBar`(远程页专用)——统一为一个组件
- SendBar 的摘要文案函数 sendBarSummary 保留复用

### 2. 媒体三 Tab 长按菜单

`MediaGridTab.kt` / `DocsTab.kt`:
- `Modifier.clickable` → `Modifier.combinedClickable(onClick, onLongClick)`
- 新增参数 `onLongClick: (path: String) -> Unit`(MediaGridTab 传 item.path,DocsTab 传 absolutePath)
- FilesScreen 在各 Tab 挂 `FileEntryMenuSheet(isRemote = false)`(复用,不改):
  本机菜单=重命名/删除/选择多项
- 菜单回调接线:
  - 重命名 → 现有 renamePath/renameOldName/showRenameDialog 状态(路径来自媒体项)
  - 删除 → deletePaths=单个+showDeleteDialog(复用 DeleteConfirmationDialog)
  - 选择多项 → 各 Tab 自己的 toggle 选中(media/docsSelected/selectedEntries)

### 3. 全部 Tab 单击=选中

FilesScreen.kt ALL Tab 的 FileEntryCard onClick:
`无选中时单击文件`(现状无反应)→ 改为 `filesViewModel.toggleSelection(entry.path)`
目录单击=进入(不变);已有选中时单击=切换选中(不变)。

### 4. 多选底部栏统一替换

- 相册/视频/文档/全部 四处 SendBar → `UnifiedSelectionBar`
- onSend 保持 showDeviceSheet = true(设备选择流程不变)
- onDelete → deletePaths = 当前 Tab 选中项路径列表 + showDeleteDialog
- onRename(单选时) → renamePath = 选中项路径 + showRenameDialog

### 5. 删除/重命名执行与刷新

- 执行:走现有 `filesViewModel.renameEntry(path, newName)` / `deleteEntries(paths)`
  (它们按 Location 分发到 local_op;媒体 Tab 项路径即本机绝对路径,语义正确)
- **相册/视频删除后刷新**:MediaPickerViewModel 加 `reload()`(重新触发 MediaStore 查询);
  FilesScreen 在 deleteEntries 确认回调里按当前 localTab 调 reload / loadEntries / collectDocs 重扫
- **确认弹窗**:复用现有 DeleteConfirmationDialog(相册删的是真照片,确认必须有)
- 失败路径:local_op 删除失败(云占位/权限)→ 现有 uiState.error Snackbar 呈现,不静默

### 6. 媒体项重命名的路径语义

MediaItem.path 是绝对路径(如 /storage/emulated/0/DCIM/xxx.jpg);
renameEntry 走 local_op RENAME 同路径语义,不涉及 MediaStore 重命名 API。
文件系统改名后 MediaStore 会自行索引新名(下次 reload 反映)。

菜单组件对接:FileEntryMenuSheet 需要 entry(name+path 两个字段);
媒体项映射 name = File(path).name、path = item.path/absolutePath,
在 FilesScreen 构造临时 FileEntryUi 传入,不改菜单组件签名。

### 7. 文档 Tab 刷新机制

现状 collectDocs 用 produceState(docsRoots) 只在首次组合时扫描。
改为 ViewModel 化或 key 化:把 docsFiles 提为
`remember { mutableStateOf }` + 显式 refreshDocs() 函数(IO 线程重扫),
删除/重命名确认回调里调用。

## 数据流(示例:视频 Tab 删除两个视频)

```
长按视频A → 菜单 → 删除 → 确认弹窗 → deleteEntries([A])
  → local_op(DELETE, A) → 成功 → mediaViewModel.reload()(MediaStore 重查,列表少 A)
                          → 失败 → uiState.error Snackbar
```

## 测试

- FilesViewModelTest:deleteEntries/renameEntry 分发已有;补媒体路径经 local_op 的 Fake 断言
- MediaPickerViewModelTest:reload() 触发重查的单测(Fake repo)
- UI 手工冒烟(装机):四 Tab 长按菜单一致、多选栏一致、删除后列表刷新、远程页不受影响

## 明确不做(YAGNI)

- MediaStore URI 删除/重命名 API(方案 B)
- 媒体 Tab 多级目录浏览(保持扁平媒体库视图)
- 批量重命名
- 远程页任何变更(现状已达标)

## 全局约束

- 纯安卓 UI,FFI/Rust 零改动
- 提交信息:中文前缀 + 空行 + Co-Authored-By: Claude <noreply@anthropic.com>
- 版本:v0.10.1(versionCode 12),三处对齐+CHANGELOG
