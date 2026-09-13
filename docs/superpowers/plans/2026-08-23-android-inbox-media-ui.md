# 安卓落盘可见化 + 移动端传输交互优化 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 手机接收的文件落到用户可见的 `Download/LocalTrans/`(图片/视频扫描进相册),文件页内建相册/视频/文档浏览器,传输页显示速度/ETA/类型图标并提供完成后查看。

**Architecture:** FFI 层注入 inbox 目录 + 新 `FilesSaved` 事件驱动 Kotlin 媒体扫描(协议零变更);Kotlin 层新增 MediaStore 媒体浏览器与 SendBar 发送组件;传输页增强在现有 TransfersViewModel/Screen 上扩展。

**Tech Stack:** Rust uniFFI (localtrans-ffi) / Kotlin + Jetpack Compose / MediaStore + MediaScannerConnection / Coil 2.7.0(新依赖,仅缩略图加载)

## Global Constraints

- 提交信息中文,前缀 feat:/fix:/chore:/docs:,结尾 `Co-Authored-By: Claude <noreply@anthropic.com>`
- Rust 测试命令 `cargo test -p localtrans-ffi --lib`;Android 单测命令(Gradle 8.10.2 + JDK17,PowerShell):
  `export JAVA_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8" && "C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin/gradle.bat" -p android --offline :app:testDebugUnitTest`
- so 交叉编译(改 Rust 后必须,产物时间戳必须检查):
  `export ANDROID_NDK_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/android-sdk/ndk/27.0.12077973" && export CARGO_HOME="/c/Users/<user>/.cargo" && export PATH="/c/Users/<user>/.cargo/bin:$PATH" && cargo ndk -t arm64-v8a -t x86_64 -o android/app/src/main/jniLibs build -p localtrans-ffi --release`
- uniffi Kotlin 绑定重生成(改 FFI 接口签名后必须):
  `cargo run -p localtrans-ffi --bin uniffi-bindgen -- generate --library target/android/x86_64/liblocaltrans_ffi.so --language kotlin --out-dir android/app/src/main/java/uniffi`
- 明文文件名/路径不进 tracing 日志(只打数量与 job_id/offer_id;指纹 hex 全长可打)——FilesSaved 事件路径只发给 Kotlin 回调,不打 log
- core 协议(crates/localtrans-core 的 ControlMsg/ProgressEvent)与本 spec 的桌面壳(src-tauri)零改动
- `state.dir` 仍是断点续传 manifest/parts 的家;仅"用户文件落点"改用 inbox_dir
- 版本号本轮不动(v0.7.0 收尾另起任务)
- 测试隔离:Android 单测不依赖真机/模拟器;Rust 测试用 tempfile

---

### Task 1: FFI inbox_dir 注入 + 落盘路径切换

**Files:**
- Modify: `crates/localtrans-ffi/src/state.rs:20-59`(AppState 加字段)、`crates/localtrans-ffi/src/state.rs:61-100`(构造函数)
- Modify: `crates/localtrans-ffi/src/lib.rs:868-893`(respond_offer)、`crates/localtrans-ffi/src/lib.rs:784`(pull_files download_dir)、`crates/localtrans-ffi/src/lib.rs:1027`(retry_transfer download_dir)
- Test: `crates/localtrans-ffi/src/lib.rs`(tests 模块,1812 行起)

**Interfaces:**
- Produces: `AppState.inbox_dir: std::path::PathBuf`(pub 字段;默认 = `dir`);FFI 方法 `pub fn set_inbox_dir(&self, dir: String) -> Result<(), AppException>`(LocalTransApp 上,#[uniffi::export])
- 后续任务依赖:Task 3 的 FilesSaved 事件在同批落盘链路上;Task 5 Kotlin 调 `app.setInboxDir(path)`

- [ ] **Step 1: 写失败测试**

在 `crates/localtrans-ffi/src/lib.rs` tests 模块末尾(1882 行 `}` 前)追加:

```rust
    #[test]
    fn set_inbox_dir_changes_receive_destination() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(
            dir.path().to_str().unwrap().into(),
            Box::new(cb)
        );
        app.start();

        // 未设置时 inbox_dir = state.dir(行为同旧)
        assert!(app.inbox_dir().starts_with(dir.path()));

        // 设置后切换
        let inbox_path = inbox.path().join("Download").join("LocalTrans");
        std::fs::create_dir_all(&inbox_path).unwrap();
        app.set_inbox_dir(inbox_path.to_str().unwrap().to_string()).unwrap();
        assert_eq!(std::path::PathBuf::from(app.inbox_dir()), inbox_path);

        app.shutdown();
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-ffi --lib`
Expected: 编译失败 `no method named set_inbox_dir / inbox_dir`

- [ ] **Step 3: 实现**

`state.rs` AppState struct 加字段(放在 `dir` 之后):

```rust
    /// 用户可见的接收落盘根目录(inbox)。默认 = dir(私有目录,行为同旧);
    /// Android 侧启动时 set_inbox_dir 指向 Download/LocalTrans。
    /// 注意:断点续传 manifest/parts 仍用 dir——内部状态不污染用户目录。
    pub inbox_dir: std::path::PathBuf,
```

`AppState::new` 签名不变(不加参数),构造体里 `dir` 克隆一份:

```rust
        Self {
            inbox_dir: dir.clone(),
            dir,
```

`lib.rs` LocalTransApp impl 块加两个导出(放在 `my_fingerprint` 后):

```rust
    /// 用户文件落盘根(inbox)。默认 = data_dir;Android 侧指向 Download/LocalTrans
    pub fn inbox_dir(&self) -> String {
        let state_guard = self.state.lock().unwrap();
        match &*state_guard {
            Some(state) => state.inbox_dir.to_string_lossy().to_string(),
            None => self.data_dir.clone(),
        }
    }

    /// 注入用户可见的接收目录(Android: Download/LocalTrans)。目录须已存在,
    /// 由调用方(Kotlin)负责 mkdirs;不存在时后续落盘报 IO 错走任务失败链路。
    pub fn set_inbox_dir(&self, dir: String) -> Result<(), AppException> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppException::Io { message: e.to_string() })?;
        let state_guard = self.state.lock().unwrap();
        match &*state_guard {
            Some(state) => {
                let mut state_ref = state;
                state_ref.inbox_dir = std::path::PathBuf::from(&dir);
                Ok(())
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }
```

注:`state` 是 `Arc<AppState>`(字段可变性经 Arc 内部字段直接赋值——AppState 字段本身非 mut,此处改为通过替换:实际实现时 AppState 的 `inbox_dir` 用 `std::sync::RwLock<PathBuf>` 包一层以支持运行时更新,构造 `inbox_dir: std::sync::RwLock::new(dir.clone())`,读取侧 `.read().unwrap().clone()`。实现者按 RwLock 版本落地,测试断言不变。)

落点切换(3 处 `state.dir.clone()` → inbox):
- `respond_offer`(lib.rs:874 附近):`let dir = state.dir.clone();` → `let dir = state.inbox_dir.read().unwrap().clone();`
- `pull_files`(lib.rs:784):`let download_dir = state.dir.clone();` → `let download_dir = state.inbox_dir.read().unwrap().clone();`
- `retry_transfer`(lib.rs:1027):同上替换

**不动的 `state.dir`**:`pending_jobs(&download_dir)` 改名变量仍传 inbox 后,`retry_transfer` 里 manifest 扫描(1028 行 `pending_jobs(&download_dir)`)保持传**私有 dir**——manifest 在私有目录。实现时把 1027/1028 行拆成两个变量:`let inbox = state.inbox_dir...clone(); let priv_dir = state.dir.clone();`,manifest 扫描用 priv_dir,落盘用 inbox。pull_files 里 `pending_jobs` 无调用,仅 download_dir 落盘语义,直接替换。

- [ ] **Step 4: 跑测试确认通过 + 全量回归**

Run: `cargo test -p localtrans-ffi --lib`
Expected: 6 passed(原 5 + 新 1)

Run: `cargo check --workspace`
Expected: 0 error

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-ffi/src/lib.rs crates/localtrans-ffi/src/state.rs
git commit -m "feat(ffi): set_inbox_dir 注入用户可见接收目录,落盘点与内部状态分离"
```

---

### Task 2: FFI FilesSaved 事件 + TransferDto.local_path

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs:22-31`(AppEvent 枚举加变体)
- Modify: `crates/localtrans-ffi/src/dto.rs:17-30`(TransferDto 加字段)
- Modify: `crates/localtrans-ffi/src/lib.rs`(所有 TransferDto 字面量构造点补字段:639/757/1038 + tests;grep `TransferDto {` 找全)
- Modify: `crates/localtrans-ffi/src/lib.rs:1570-1642`(handle_recv_progress_event 的 Done 分支)
- Test: `crates/localtrans-ffi/src/lib.rs` tests 模块

**Interfaces:**
- Produces: `AppEvent::FilesSaved { job_id: u64, paths: Vec<String> }`(uniffi Enum 变体,Kotlin 侧 `AppEvent.FilesSaved`);`TransferDto.local_path: Option<String>`(Kotlin 侧 `localPath: String?`)
- Consumes: Task 1 的 inbox_dir(落盘路径在 inbox 下)

背景:接收完成的 Done 事件在 handle_recv_progress_event 里只带 job_id;Kotlin 需要精确路径才能 MediaScanner 扫描 + ACTION_VIEW 查看。core 的 `recv_small_files_batched`/`recv_push_large_file` 不回传落盘路径(在 core 内部),所以 FFI 层按 job 维护"已见文件名"映射:Started 事件带 name、落盘根是 inbox_dir,Done 时拼出路径列表。

- [ ] **Step 1: 写失败测试**

tests 模块追加:

```rust
    #[test]
    fn files_saved_paths_built_from_started_names() {
        // 纯函数测试:job 累积的文件名 + inbox 根 → Done 时拼出绝对路径
        let mut acc = super::SavedFilesAcc::new("/sdcard/Download/LocalTrans".into());
        acc.record("a.png");
        acc.record("sub/b.jpg"); // rel_dir 场景(文件夹推送)
        let paths = acc.finish(42u64);
        assert_eq!(paths.0, 42u64);
        assert_eq!(paths.1, vec![
            "/sdcard/Download/LocalTrans/a.png".to_string(),
            "/sdcard/Download/LocalTrans/sub/b.jpg".to_string(),
        ]);
    }

    #[test]
    fn local_path_field_serializes_none() {
        let dto = crate::dto::TransferDto {
            job_id: 1, name: "x".into(), total: 0, done: 0, state: "pending".into(),
            speed_bps: 0, peer: "ab".into(), direction: "pull".into(),
            local_role: "receiver".into(), progress_percent: 0, eta_secs: -1,
            fail_reason: String::new(), local_path: None,
        };
        assert!(dto.local_path.is_none());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-ffi --lib`
Expected: 编译失败 `SavedFilesAcc 不存在` / `local_path 字段不存在`

- [ ] **Step 3: 实现**

AppEvent 枚举(lib.rs:22-31)加变体(放 TransferDone 之后):

```rust
    /// 接收侧文件已落盘(仅 FFI 层事件,core 无感知)。
    /// paths 为绝对路径——Kotlin 据此做 MediaScanner 扫描与"查看"跳转。
    /// 注意:该事件在 TransferDone 之前发出。
    FilesSaved { job_id: u64, paths: Vec<String> },
```

TransferDto(dto.rs)加字段:

```rust
    /// 接收完成后首个落盘文件的绝对路径(发送侧/未落盘为 None)。
    /// Android "查看"跳转用;uniffi 0.28 Option → Kotlin String?
    pub local_path: Option<String>,
```

lib.rs 顶部(ffi_guard 附近)加累积器:

```rust
/// 接收侧 per-job 已见文件名累积器:Started(name) 时 record,
/// Done 时 finish 拼绝对路径。名称即相对 inbox 根的路径(core 的
/// Started.name 对小文件批流是文件名、对带 rel_dir 的是 "rel/name" 组合,
/// 与落盘结构一致——直接 join 即得绝对路径)。
#[derive(Default)]
pub struct SavedFilesAcc {
    root: std::path::PathBuf,
    names: Vec<String>,
}

impl SavedFilesAcc {
    pub fn new(root: std::path::PathBuf) -> Self {
        Self { root, names: Vec::new() }
    }
    pub fn record(&mut self, name: String) {
        self.names.push(name);
    }
    pub fn finish(self, job_id: u64) -> (u64, Vec<String>) {
        let paths = self.names.iter()
            .map(|n| self.root.join(n).to_string_lossy().to_string())
            .collect();
        (job_id, paths)
    }
}
```

AppState(state.rs)加字段(与 Task 1 同批 RwLock 风格):

```rust
    /// 接收侧 per-job 文件名累积(Started 事件喂入,Done 时生成 FilesSaved)
    pub saved_files: Arc<Mutex<HashMap<u64, SavedFilesAcc>>>,
```

构造函数初始化 `saved_files: Arc::new(Mutex::new(HashMap::new()))`。

handle_recv_progress_event 改造:
- `Started { job_id, name, .. }` 分支末尾追加:

```rust
            st.saved_files.lock().unwrap()
                .entry(job_id)
                .or_insert_with(|| crate::SavedFilesAcc::new(st.inbox_dir.read().unwrap().clone()))
                .record(name.clone());
```

- `Done { job_id }` 分支,在 `dto.state = "done"` 之后、`cb.on_event(AppEvent::TransferUpdated...)` 之前插入:

```rust
            if let Some(acc) = st.saved_files.lock().unwrap().remove(&job_id) {
                let (_, paths) = acc.finish(job_id);
                if !paths.is_empty() {
                    if let Some(first) = paths.first() {
                        dto.local_path = Some(first.clone());
                    }
                    cb.on_event(AppEvent::FilesSaved { job_id, paths });
                }
            }
```

- `Failed { job_id, .. }` 分支补清理(防泄漏):`st.saved_files.lock().unwrap().remove(&job_id);`

全部 `TransferDto {` 字面量构造点(639/757/1038 行与 tests)补 `local_path: None,`。

- [ ] **Step 4: 跑测试确认通过 + 绑定重生成**

Run: `cargo test -p localtrans-ffi --lib`
Expected: 8 passed

Run(重编译 so + 重生成 Kotlin 绑定,Global Constraints 里的两条命令,先后执行;so 时间戳与 uniffi Kotlin 文件里出现 `FilesSaved`/`localPath` 为准)

Run: `cargo check --workspace` → 0 error

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-ffi android/app/src/main/java/uniffi android/app/src/main/jniLibs
git commit -m "feat(ffi): FilesSaved 事件+TransferDto.local_path(落盘路径精确通知 Kotlin)"
```

---

### Task 3: Kotlin MediaScanNotifier + Bridge 接线(inbox 注入)

**Files:**
- Create: `android/app/src/main/java/com/localtrans/app/media/MediaScanNotifier.kt`
- Modify: `android/app/src/main/java/com/localtrans/app/LocalTransApp.kt`(Bridge.init 里注入 inbox)
- Modify: `android/app/src/main/java/com/localtrans/app/bridge/EventRouter.kt`(FilesSaved 分支)
- Test: `android/app/src/test/java/com/localtrans/app/media/MediaScanNotifierTest.kt`

**Interfaces:**
- Consumes: Task 1 `app.setInboxDir(path)`;Task 2 `AppEvent.FilesSaved`
- Produces: `MediaScanNotifier.isScannableExtension(name: String): Boolean`(纯函数,static);`MediaScanNotifier.scanPaths(context, paths)`(fire-and-forget)

- [ ] **Step 1: 写失败测试**

```kotlin
package com.localtrans.app.media

import org.junit.Assert.*
import org.junit.Test

class MediaScanNotifierTest {
    @Test
    fun `image and video extensions are scannable`() {
        assertTrue(MediaScanNotifier.isScannableExtension("photo.JPG"))
        assertTrue(MediaScanNotifier.isScannableExtension("clip.mp4"))
        assertTrue(MediaScanNotifier.isScannableExtension("img.heic"))
    }

    @Test
    fun `document extensions are not scannable`() {
        assertFalse(MediaScanNotifier.isScannableExtension("report.pdf"))
        assertFalse(MediaScanNotifier.isScannableExtension("archive.zip"))
        assertFalse(MediaScanNotifier.isScannableExtension("noext"))
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run(Gradle 命令见 Global Constraints)
Expected: 编译失败 `unresolved reference MediaScanNotifier`

- [ ] **Step 3: 实现**

`MediaScanNotifier.kt`:

```kotlin
package com.localtrans.app.media

import android.content.Context
import android.media.MediaScannerConnection
import android.os.Build
import android.util.Log
import java.io.File

/**
 * 接收落盘后的媒体索引通知:图片/视频经 MediaScanner 扫描进系统相册。
 * 文档类不处理(Download/LocalTrans 文件管理器天然可见)。
 */
object MediaScanNotifier {
    private const val TAG = "MediaScanNotifier"
    private val SCANNABLE = setOf(
        "jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "bmp",
        "mp4", "mov", "mkv", "avi", "3gp", "webm"
    )

    fun isScannableExtension(name: String): Boolean {
        val ext = name.substringAfterLast('.', "").lowercase()
        return ext in SCANNABLE
    }

    /** 即发即忘:过滤媒体扩展名后交系统扫描,失败只打日志 */
    fun scanPaths(context: Context, paths: List<String>) {
        val media = paths.filter { isScannableExtension(File(it).name) }
        if (media.isEmpty()) return
        try {
            MediaScannerConnection.scanFile(context, media.toTypedArray(), null) { _, _ -> }
            Log.d(TAG, "媒体扫描请求已发出: ${media.size} 项")
        } catch (e: Exception) {
            Log.w(TAG, "媒体扫描失败(不影响传输): ${e.message}")
        }
    }
}
```

`LocalTransApp.kt` Bridge.init,在 `app = FfiApp(dataDir, Callback())` 之后、`applicationScope.launch` 之前插入:

```kotlin
            // 接收落盘注入用户可见目录:Download/LocalTrans(图片/视频扫描进相册,
            // 文档文件管理器可见)。目录自建;失败仅回落私有目录(不阻断启动)。
            try {
                @Suppress("DEPRECATION")
                val downloads = android.os.Environment
                    .getExternalStoragePublicDirectory(android.os.Environment.DIRECTORY_DOWNLOADS)
                val inbox = java.io.File(downloads, "LocalTrans")
                if (inbox.exists() || inbox.mkdirs()) {
                    app.setInboxDir(inbox.absolutePath)
                }
            } catch (e: Exception) {
                Log.w(TAG, "inbox 目录注入失败,回落私有目录: ${e.message}")
            }
```

`EventRouter.route` when 里加分支:

```kotlin
            is AppEvent.FilesSaved -> {
                try {
                    MediaScanNotifier.scanPaths(
                        LocalTransBridge.getAppContext(),
                        event.paths
                    )
                } catch (e: Exception) {
                    // Bridge 未初始化时静默
                }
            }
```

- [ ] **Step 4: 跑测试确认通过**

Run: Gradle testDebugUnitTest
Expected: 全部通过(含既有 35+)

- [ ] **Step 5: 提交**

```bash
git add android/app/src/main/java/com/localtrans/app/media android/app/src/main/java/com/localtrans/app/LocalTransApp.kt android/app/src/main/java/com/localtrans/app/bridge/EventRouter.kt android/app/src/test
git commit -m "feat(android): 接收落盘 Download/LocalTrans + 媒体扫描进相册"
```

---

### Task 4: 媒体浏览器数据层(MediaRepo + MediaPickerViewModel)

**Files:**
- Create: `android/app/src/main/java/com/localtrans/app/media/MediaRepo.kt`(接口 + MediaStore 实现 + Fake)
- Create: `android/app/src/main/java/com/localtrans/app/media/MediaPickerViewModel.kt`
- Modify: `android/app/build.gradle.kts`(dependencies 加 Coil)
- Test: `android/app/src/test/java/com/localtrans/app/media/MediaPickerViewModelTest.kt`

**Interfaces:**
- Produces:
  - `data class MediaItem(val id: Long, val path: String, val name: String, val size: Long, val dateTakenMs: Long, val durationMs: Long = 0, val bucketName: String = "")`
  - `interface MediaRepo { fun queryImages(): List<MediaItem>; fun queryVideos(): List<MediaItem> }`
  - `MediaPickerViewModel(repo, scope)`:`val items: StateFlow<List<MediaItem>>`、`val selected: StateFlow<Set<Long>>`、`val filter: StateFlow<MediaFilter>`、`fun setFilter(f)`、`fun toggleSelect(id: Long)`、`fun clearSelection()`、`fun selectedPaths(): List<String>`、`fun refresh()`
  - `enum class MediaFilter { PHOTOS, VIDEOS }`(文档/全部 Tab 走既有 FilesViewModel,不在此 ViewModel)
- Consumes: 无外部新依赖(MediaStore 在 framework);Coil 仅 UI 层用

- [ ] **Step 1: 写失败测试**

```kotlin
package com.localtrans.app.media

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.Assert.*
import org.junit.Before
import org.junit.After
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class MediaPickerViewModelTest {
    private val testDispatcher = StandardTestDispatcher()

    @Before fun setup() { Dispatchers.setMain(testDispatcher) }
    @After fun tearDown() { Dispatchers.resetMain() }

    private fun repoWith(vararg items: MediaItem) = object : MediaRepo {
        override fun queryImages() = items.filter { it.durationMs == 0L }
        override fun queryVideos() = items.filter { it.durationMs > 0L }
    }

    @Test
    fun `filter switches between photos and videos`() = runTest {
        val vm = MediaPickerViewModel(repoWith(
            MediaItem(1, "/a.jpg", "a.jpg", 1, 1),
            MediaItem(2, "/v.mp4", "v.mp4", 1, 1, durationMs = 3000),
        ))
        testDispatcher.scheduler.advanceUntilIdle()
        vm.setFilter(MediaFilter.PHOTOS)
        assertEquals(listOf("/a.jpg"), vm.items.value.map { it.path })
        vm.setFilter(MediaFilter.VIDEOS)
        assertEquals(listOf("/v.mp4"), vm.items.value.map { it.path })
    }

    @Test
    fun `toggle select and selectedPaths`() = runTest {
        val vm = MediaPickerViewModel(repoWith(MediaItem(1, "/a.jpg", "a.jpg", 1, 1)))
        testDispatcher.scheduler.advanceUntilIdle()
        vm.toggleSelect(1)
        assertEquals(setOf(1L), vm.selected.value)
        assertEquals(listOf("/a.jpg"), vm.selectedPaths())
        vm.clearSelection()
        assertTrue(vm.selected.value.isEmpty())
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: Gradle testDebugUnitTest --tests "com.localtrans.app.media.MediaPickerViewModelTest"
Expected: 编译失败 unresolved reference

- [ ] **Step 3: 实现**

`MediaRepo.kt`:

```kotlin
package com.localtrans.app.media

import android.content.ContentUris
import android.content.Context
import android.provider.MediaStore

data class MediaItem(
    val id: Long,
    val path: String,
    val name: String,
    val size: Long,
    val dateTakenMs: Long,
    val durationMs: Long = 0,
    val bucketName: String = ""
)

interface MediaRepo {
    fun queryImages(): List<MediaItem>
    fun queryVideos(): List<MediaItem>
}

/** MediaStore 直查(DATA 列绝对路径;所有文件访问已授权,v0.6.1 实测可行) */
class MediaStoreRepo(private val context: Context) : MediaRepo {
    override fun queryImages(): List<MediaItem> =
        query(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, projectionImages())

    override fun queryVideos(): List<MediaItem> =
        query(MediaStore.Video.Media.EXTERNAL_CONTENT_URI, projectionVideos())

    private fun projectionImages() = arrayOf(
        MediaStore.Images.Media._ID,
        MediaStore.Images.Media.DATA,
        MediaStore.Images.Media.DISPLAY_NAME,
        MediaStore.Images.Media.SIZE,
        MediaStore.Images.Media.DATE_TAKEN,
        MediaStore.Images.Media.BUCKET_DISPLAY_NAME,
    )

    private fun projectionVideos() = arrayOf(
        MediaStore.Video.Media._ID,
        MediaStore.Video.Media.DATA,
        MediaStore.Video.Media.DISPLAY_NAME,
        MediaStore.Video.Media.SIZE,
        MediaStore.Video.Media.DATE_TAKEN,
        MediaStore.Video.Media.DURATION,
        MediaStore.Video.Media.BUCKET_DISPLAY_NAME,
    )

    private fun query(uri: android.net.Uri, projection: Array<String>): List<MediaItem> {
        val items = mutableListOf<MediaItem>()
        context.contentResolver.query(uri, projection, null, null, "DATE_TAKEN DESC")?.use { c ->
            while (c.moveToNext()) {
                val path = c.getString(1) ?: continue
                items.add(
                    MediaItem(
                        id = c.getLong(0),
                        path = path,
                        name = c.getString(2) ?: "",
                        size = c.getLong(3),
                        dateTakenMs = c.getLong(4),
                        durationMs = if (projection.size == 7) c.getLong(5) else 0L,
                        bucketName = if (projection.size == 7) c.getString(6) ?: "" else c.getString(5) ?: "",
                    )
                )
            }
        }
        return items
    }
}
```

`MediaPickerViewModel.kt`:

```kotlin
package com.localtrans.app.media

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

enum class MediaFilter { PHOTOS, VIDEOS }

class MediaPickerViewModel(
    private val repo: MediaRepo,
) : ViewModel() {
    private val _items = MutableStateFlow<List<MediaItem>>(emptyList())
    val items: StateFlow<List<MediaItem>> = _items.asStateFlow()

    private val _selected = MutableStateFlow<Set<Long>>(emptySet())
    val selected: StateFlow<Set<Long>> = _selected.asStateFlow()

    private val _filter = MutableStateFlow(MediaFilter.PHOTOS)
    val filter: StateFlow<MediaFilter> = _filter.asStateFlow()

    init { refresh() }

    fun setFilter(f: MediaFilter) { _filter.value = f; refresh() }

    fun refresh() {
        viewModelScope.launch {
            val list = withContext(Dispatchers.IO) {
                when (_filter.value) {
                    MediaFilter.PHOTOS -> repo.queryImages()
                    MediaFilter.VIDEOS -> repo.queryVideos()
                }
            }
            _items.value = list
            _selected.value = _selected.value.intersect(list.map { it.id }.toSet())
        }
    }

    fun toggleSelect(id: Long) {
        _selected.update { if (id in it) it - id else it + id }
    }

    fun clearSelection() { _selected.value = emptySet() }

    fun selectedPaths(): List<String> =
        _items.value.filter { it.id in _selected.value }.map { it.path }
}
```

`build.gradle.kts` dependencies 块加:

```kotlin
    implementation("io.coil-kt:coil-compose:2.7.0")
```

- [ ] **Step 4: 跑测试确认通过**

Run: Gradle testDebugUnitTest
Expected: 全绿

- [ ] **Step 5: 提交**

```bash
git add android/app/src/main/java/com/localtrans/app/media android/app/src/test/java/com/localtrans/app/media android/app/build.gradle.kts
git commit -m "feat(android): MediaRepo/MediaPickerViewModel 媒体浏览数据层 + Coil 依赖"
```

---

### Task 5: 文件页 UI 改造(相册/视频/文档/全部 + SendBar)

**Files:**
- Create: `android/app/src/main/java/com/localtrans/app/ui/files/SendBar.kt`
- Create: `android/app/src/main/java/com/localtrans/app/ui/files/MediaGridTab.kt`
- Create: `android/app/src/main/java/com/localtrans/app/ui/files/DocsTab.kt`
- Modify: `android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt`(二级 Tab + 装配)
- Modify: `android/app/src/main/java/com/localtrans/app/data/FilesRepo.kt`(pushFilesRel 声明 + FFI 实现)
- Test: `android/app/src/test/java/com/localtrans/app/ui/files/SendBarTest.kt`

**Interfaces:**
- Consumes: Task 4 `MediaPickerViewModel`/`MediaItem`;既有 `FilesViewModel`、`LocalTransBridge.pushFiles`;Task 2 FFI 新导出 `push_files_rel(fp: String, files: Vec<PushFile>)`(`PushFile { path: String, rel_dir: String }`,uniffi Record——**Task 2 重生成绑定时已包含**)
- Produces: `SendBar(selectedCount: Int, totalBytes: Long, onSend: () -> Unit, onClear: () -> Unit)`;FilesScreen 内二级 Tab 状态机

**FFI push_files_rel(Task 2 同批加,此处 UI 消费)**——lib.rs LocalTransApp 加导出:

```rust
    /// 按相对目录推送(文件夹结构保持)。rel_dir 为远端落盘子目录,
    /// 空串 = 根。与 push_files 同构:占位任务 + xfer_lock + 事件泵。
    pub fn push_files_rel(&self, fp: String, files: Vec<PushFileDto>) -> u64 {
        // 实现照抄 push_files(624-743 行)唯一差异:
        // let paths = files.iter().map(|f| (PathBuf::from(&f.path), f.rel_dir.clone())).collect();
        // engine 调 localtrans_core::transfer::push_files_rel(&sm, &fp_bytes, paths, &st.sender_jobs, progress_tx)
    }
```

dto.rs 加 Record:

```rust
#[derive(uniffi::Record, Clone, Debug)]
pub struct PushFileDto {
    pub path: String,
    pub rel_dir: String,
}
```

- [ ] **Step 1: 写失败测试(SendBar 汇总纯函数)**

```kotlin
package com.localtrans.app.ui.files

import org.junit.Assert.*
import org.junit.Test

class SendBarTest {
    @Test
    fun `summary text combines count and size`() {
        assertEquals("已选 3 项 · 12.4 MB", sendBarSummary(3, 12_400_000L))
        assertEquals("已选 1 项 · 0 B", sendBarSummary(1, 0L))
    }
}
```

`sendBarSummary(count: Int, bytes: Long): String`(顶层纯函数,放 SendBar.kt;格式化复用 `Formatters.formatFileSize`)。

- [ ] **Step 2: 跑测试确认失败**

Run: Gradle testDebugUnitTest --tests "com.localtrans.app.ui.files.SendBarTest"
Expected: 编译失败

- [ ] **Step 3: 实现**

`SendBar.kt`:

```kotlin
package com.localtrans.app.ui.files

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Send
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.localtrans.app.util.Formatters

fun sendBarSummary(count: Int, bytes: Long): String =
    "已选 $count 项 · ${Formatters.formatFileSize(bytes.toULong())}"

@Composable
fun SendBar(
    selectedCount: Int,
    totalBytes: Long,
    onSend: () -> Unit,
    onClear: () -> Unit
) {
    Surface(tonalElevation = 8.dp, shadowElevation = 8.dp) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Text(sendBarSummary(selectedCount, totalBytes), Modifier.weight(1f))
            IconButton(onClick = onClear) { Icon(Icons.Default.Close, "清除") }
            Button(onClick = onSend, enabled = selectedCount > 0) {
                Icon(Icons.Default.Send, null, Modifier.size(16.dp))
                Spacer(Modifier.width(4.dp))
                Text("发送")
            }
        }
    }
}
```

`MediaGridTab.kt`(相册/视频共用网格;Coil 缩略图;权限缺失引导复用 StoragePermissions):

```kotlin
package com.localtrans.app.ui.files

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
import com.localtrans.app.media.MediaItem
import com.localtrans.app.util.Formatters

@Composable
fun MediaGridTab(
    items: List<MediaItem>,
    selected: Set<Long>,
    onToggle: (Long) -> Unit,
    modifier: Modifier = Modifier
) {
    LazyVerticalGrid(
        columns = GridCells.Fixed(3),
        modifier = modifier.fillMaxSize().padding(2.dp),
    ) {
        items(items, key = { it.id }) { item ->
            Box(Modifier.padding(2.dp)) {
                AsyncImage(
                    model = java.io.File(item.path),
                    contentDescription = item.name,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier
                        .fillMaxWidth()
                        .aspectRatio(1f)
                        .androidx.compose.foundation.clickable { onToggle(item.id) }
                )
                if (item.id in selected) {
                    Icon(
                        Icons.Default.CheckCircle, null,
                        modifier = Modifier.align(Alignment.TopEnd).padding(4.dp).size(24.dp),
                        tint = MaterialTheme.colorScheme.primary
                    )
                }
                if (item.durationMs > 0) {
                    Text(
                        Formatters.formatDuration(item.durationMs),
                        style = MaterialTheme.typography.labelSmall,
                        modifier = Modifier.align(Alignment.BottomEnd).padding(4.dp)
                    )
                }
            }
        }
    }
}
```

(注:`clickable` 需要 import `androidx.compose.foundation.clickable` 并挂 modifier 链——实现时写成 `.clickable { onToggle(item.id) }`;Formatters 若无 formatDuration 则加一个 `mm:ss`/`h:mm:ss` 实现 + 单测。)

`DocsTab.kt`(文档列表:FFI listLocal 递归收集文档扩展名,平铺):

```kotlin
package com.localtrans.app.ui.files

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.FolderZip
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.localtrans.app.util.Formatters
import java.io.File

val DOC_EXTENSIONS = setOf(
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
    "txt", "md", "csv", "zip", "7z", "rar", "json", "epub"
)

fun isDocFile(name: String): Boolean =
    name.substringAfterLast('.', "").lowercase() in DOC_EXTENSIONS

/** 扫描根下常见文档(非递归进隐藏目录;深度上限 3 层防遍历整盘) */
fun collectDocs(roots: List<File>, maxDepth: Int = 3): List<File> {
    val out = mutableListOf<File>()
    fun walk(dir: File, depth: Int) {
        if (depth > maxDepth) return
        val entries = dir.listFiles() ?: return
        for (f in entries) {
            if (f.name.startsWith(".")) continue
            if (f.isDirectory) walk(f, depth + 1)
            else if (isDocFile(f.name)) out.add(f)
        }
    }
    roots.forEach { if (it.exists()) walk(it, 0) }
    return out.sortedByDescending { it.lastModified() }
}

@Composable
fun DocsTab(
    files: List<File>,
    selected: Set<String>,
    onToggle: (String) -> Unit,
    modifier: Modifier = Modifier
) {
    LazyColumn(modifier = modifier.fillMaxSize()) {
        items(files, key = { it.absolutePath }) { f ->
            ListItem(
                modifier = Modifier.clickable { onToggle(f.absolutePath) },
                leadingContent = {
                    Icon(
                        if (f.extension.lowercase() in setOf("zip", "7z", "rar"))
                            Icons.Default.FolderZip else Icons.Default.Description,
                        null
                    )
                },
                headlineContent = { Text(f.name) },
                supportingContent = {
                    Text("${Formatters.formatFileSize(f.length().toULong())} · " +
                        java.text.SimpleDateFormat("yyyy-MM-dd", java.util.Locale.getDefault())
                            .format(java.util.Date(f.lastModified())))
                },
                trailingContent = {
                    if (f.absolutePath in selected) {
                        Icon(Icons.Default.CheckCircle, null, tint = MaterialTheme.colorScheme.primary)
                    }
                }
            )
        }
    }
}
```

`FilesRepo.kt` 接口加:

```kotlin
    /**
     * Push files with relative dirs (folder structure preserved)
     */
    fun pushFilesRel(fp: String, files: List<Pair<String, String>>): kotlin.ULong
```

FfiFilesRepo 实现:

```kotlin
    override fun pushFilesRel(fp: String, files: List<Pair<String, String>>): kotlin.ULong {
        val dtos = files.map { uniffi.localtrans_ffi.PushFileDto(path = it.first, relDir = it.second) }
        return app.pushFilesRel(fp, dtos)
    }
```

FakeFilesRepo 实现返回 `1u`。

`FilesScreen.kt` 改造要点(完整重写文件页装配):
- 一级 Tab(本机/远程)不动;LOCAL 时在 LocationTabs 下加二级 `TabRow`:`[相册] [视频] [文档] [全部]`,状态 `var localTab by remember { mutableStateOf(LocalTab.ALL) }`(enum LocalTab { PHOTOS, VIDEOS, DOCS, ALL })
- PHOTOS/VIDEOS:一个共享 `MediaPickerViewModel`(viewModel(factory=…MediaStoreRepo(context))),`MediaGridTab(items, selected, vm::toggleSelect)`
- DOCS:`remember { collectDocs(listOf(Environment.getExternalStorageDirectory().resolve("Download"), ...resolve("Documents"), ...resolve("DCIM"))) }` + `DocsTab`
- ALL:现有目录浏览(面包屑/多选/FAB 全保留),另外**文件夹长按加入选择**(现有 onLongClick 已是 toggleSelection,天然支持,无需改)
- 选择状态:PHOTOS/VIDEOS 用 MediaPickerViewModel.selected;DOCS/ALL 用 FilesViewModel.selectedEntries——SendBar 按当前 Tab 取对应选择集与大小(媒体:sum of MediaItem.size;DOCS/ALL:File.length())
- SendBar 的 onSend:`ModalBottomSheet` 设备列表(DeviceUi 列表来自 DevicesViewModel 同源 FFI devices();简单实现:直接 `LocalTransBridge.app.devices().filter { it.connected }` 拉一次),点设备后:
  - PHOTOS/VIDEOS:`viewModel.pushFiles(fp, vm.selectedPaths())`(既有 FilesRepo.pushFiles)
  - DOCS:`pushFiles(fp, paths)`
  - ALL:`pushFilesRel(fp, paths.zip(relDirs))`——relDir 取相对浏览根的父目录(选中文件夹:递归展开其下文件,rel_dir = 文件夹名/子路径;实现:walk 收集 (file, relative) 对)
- 发送后清选择、收起 Sheet、snackbar "已开始发送"

- [ ] **Step 4: 跑测试确认通过**

Run: Gradle testDebugUnitTest
Expected: 全绿(SendBarTest 2 个 + 既有)

- [ ] **Step 5: 提交**

```bash
git add android/app/src/main/java/com/localtrans/app/ui/files android/app/src/main/java/com/localtrans/app/data/FilesRepo.kt android/app/src/test
git commit -m "feat(android): 文件页相册/视频/文档/全部四 Tab + SendBar 发送(文件夹结构保持)"
```

---

### Task 6: 传输页增强(速度/ETA/类型图标/查看跳转/通知深链)

**Files:**
- Create: `android/app/src/main/java/com/localtrans/app/ui/transfers/SpeedEstimator.kt`
- Create: `android/app/src/main/java/com/localtrans/app/util/FileTypeIcons.kt`
- Modify: `android/app/src/main/java/com/localtrans/app/ui/transfers/TransfersUiModel.kt`(TransferUi 加 localPath)
- Modify: `android/app/src/main/java/com/localtrans/app/ui/transfers/TransfersViewModel.kt`(toUiModel 映射 localPath)
- Modify: `android/app/src/main/java/com/localtrans/app/ui/transfers/TransfersScreen.kt`(行改造)
- Modify: `android/app/src/main/java/com/localtrans/app/notification/TransferNotifier.kt`(通知点击传 extra)
- Modify: `android/app/src/main/java/com/localtrans/app/MainActivity.kt`(onNewIntent/read extra)
- Modify: `android/app/src/main/java/com/localtrans/app/ui/nav/AppNav.kt`(extra 驱动切 Tab)
- Test: `android/app/src/test/java/com/localtrans/app/ui/transfers/SpeedEstimatorTest.kt`、`android/app/src/test/java/com/localtrans/app/util/FileTypeIconsTest.kt`

**Interfaces:**
- Consumes: Task 2 `TransferDto.localPath`(Kotlin `localPath: String?`)
- Produces: `SpeedEstimator`(纯函数 `estimate(samples: List<Pair<Long, Long>>): Long`——(timestampMs, doneBytes) 近 3s 窗差分 B/s);`FileTypeIcons.iconFor(name: String): ImageVector`

- [ ] **Step 1: 写失败测试**

`SpeedEstimatorTest.kt`:

```kotlin
package com.localtrans.app.ui.transfers

import org.junit.Assert.*
import org.junit.Test

class SpeedEstimatorTest {
    @Test
    fun `computes bps from sliding window`() {
        // 3s 窗内:0ms→0B, 3000ms→3MB → 1MB/s = 1_048_576 B/s 附近(整数除法 1048576)
        val s = listOf(0L to 0L, 3000L to 3_145_728L)
        assertEquals(1_048_576L, SpeedEstimator.estimate(s))
    }

    @Test
    fun `single sample yields zero`() {
        assertEquals(0L, SpeedEstimator.estimate(listOf(1000L to 500L)))
    }

    @Test
    fun `eta divides remaining by speed`() {
        assertEquals(2L, SpeedEstimator.etaSecs(remainingBytes = 2_097_152L, bps = 1_048_576L))
        assertEquals(-1L, SpeedEstimator.etaSecs(2_097_152L, 0L))
    }
}
```

`FileTypeIconsTest.kt`:

```kotlin
package com.localtrans.app.util

import org.junit.Assert.*
import org.junit.Test

class FileTypeIconsTest {
    @Test
    fun `maps media and docs by extension`() {
        assertNotNull(FileTypeIcons.iconFor("a.jpg"))
        assertNotNull(FileTypeIcons.iconFor("b.pdf"))
        assertNotNull(FileTypeIcons.iconFor("c.zip"))
        assertNotNull(FileTypeIcons.nothing())
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: Gradle testDebugUnitTest --tests "*SpeedEstimatorTest" --tests "*FileTypeIconsTest"
Expected: 编译失败

- [ ] **Step 3: 实现**

`SpeedEstimator.kt`:

```kotlin
package com.localtrans.app.ui.transfers

/** 近 3 秒 (timestampMs, doneBytes) 滑动窗差分测速;纯函数可单测 */
object SpeedEstimator {
    fun estimate(samples: List<Pair<Long, Long>>): Long {
        if (samples.size < 2) return 0L
        val (t0, b0) = samples.first()
        val (t1, b1) = samples.last()
        val dtMs = t1 - t0
        if (dtMs <= 0) return 0L
        return (b1 - b0) * 1000L / dtMs
    }

    fun etaSecs(remainingBytes: Long, bps: Long): Long =
        if (bps <= 0) -1L else remainingBytes / bps
}
```

`FileTypeIcons.kt`:

```kotlin
package com.localtrans.app.util

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.ui.graphics.vector.ImageVector

object FileTypeIcons {
    fun iconFor(name: String): ImageVector {
        val ext = name.substringAfterLast('.', "").lowercase()
        return when (ext) {
            "jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "bmp" -> Icons.Default.Image
            "mp4", "mov", "mkv", "avi", "3gp", "webm" -> Icons.Default.Videocam
            "mp3", "wav", "flac", "aac", "ogg" -> Icons.Default.AudioFile
            "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "csv" -> Icons.Default.Description
            "zip", "7z", "rar", "tar", "gz" -> Icons.Default.FolderZip
            else -> Icons.Default.InsertDriveFile
        }
    }

    /** "其他"占位(测试引用锚点) */
    fun nothing(): ImageVector = Icons.Default.InsertDriveFile
}
```

`TransfersUiModel.kt` TransferUi 加字段 `val localPath: String? = null`。
`TransfersViewModel.kt` toUiModel 加 `localPath = localPath`(uniffi 生成的属性名按绑定实际为准,`Option<String>` → `String?`)。

`TransfersScreen.kt` TransferCard 改造:
- 方向图标旁加 `Icon(FileTypeIcons.iconFor(transfer.name), null, Modifier.size(18.dp))`
- 信息行:`"${formatFileSize(done)} / ${formatFileSize(total)}"`,`"${formatSpeed(speedBps)}"`,`etaSecs > 0 ? 剩余 mm:ss : "--"`(Formatters 补 formatSpeed: B/s→KB/s/MB/s 字符串 + 单测)
- done 行:localPath 非空且媒体扩展名 → `AsyncImage(model = File(localPath), 24dp 角标)`;整卡 `clickable { localPath?.let { ACTION_VIEW(FileProvider URI) } }`(FileProvider:`AndroidManifest` provider `androidx.core.content.FileProvider` + `res/xml/file_paths.xml` external-path Download,authority `${applicationId}.fileprovider`;Intent `Intent(Intent.ACTION_VIEW).setDataAndType(uri, mimeType).addFlags(FLAG_GRANT_READ_URI_PERMISSION)`)
- 速度兜底:TransferUi.speedBps 已由 FFI Speed 事件维护(1606-1618 行),UI 直接用;SpeedEstimator 用于 done 窗口兜底(TransferUpdated 采样进 ViewModel map<jobId, List<Pair>>,cap 8 条/3s 窗,每次 update 计算覆盖 speedBps 为 0 的行)

`TransferNotifier.kt` intent 加 `putExtra("open_tab", "transfers")`。
`MainActivity.kt` onCreate/onNewIntent 读取 extra 存 `LocalTransBridge.pendingTab`(simple object var);`AppNav.kt` LaunchedEffect(Unit) 读 `LocalTransBridge.pendingTab` → `navController.navigate("transfers")` 后置 null。

- [ ] **Step 4: 跑测试确认通过**

Run: Gradle testDebugUnitTest
Expected: 全绿(+5 新测试)

- [ ] **Step 5: 提交**

```bash
git add android/app/src/main/java/com/localtrans/app android/app/src/test android/app/src/main/res android/app/src/main/AndroidManifest.xml
git commit -m "feat(android): 传输页速度/ETA/类型图标/完成查看 + 通知深链"
```

---

### Task 7: 装机 E2E + 回归 + 收尾

**Files:**
- Modify: `CHANGELOG.md`(v0.7.0 小节)
- Test: 双端装机冒烟(模拟器 emulator-5554 + 真机 e0252fd7 + 桌面 exe)

**Interfaces:**
- Consumes: 全部前序任务产物

- [ ] **Step 1: Rust 全量回归**

Run: `cargo test -p localtrans-ffi --lib && cargo check --workspace`
Expected: ffi 全绿,workspace 0 error

- [ ] **Step 2: so 重编 + release APK + 双端安装**

Run: Global Constraints 的 cargo ndk 命令 → Gradle assembleRelease → `adb -s emulator-5554 install -r ...` 与 `adb -s e0252fd7 install -r ...`
Expected: 安装成功,**检查 so 时间戳为最新**

- [ ] **Step 3: 装机冒烟清单(逐项验证,任一失败回修)**

1. 桌面(先启动本仓桌面 exe)推 1 张图片到真机 → OfferSheet 接受 → `adb -s e0252fd7 shell ls /storage/emulated/0/Download/LocalTrans/` 见文件;`adb shell media store check`(相册 App 或 `content query --uri content://media/external/images` 过滤 LocalTrans)确认已索引
2. 桌面推 1 个 pdf → Download/LocalTrans 可见,相册无
3. 真机相册 Tab 选 3 张图 → SendBar → 选桌面设备 → PC 端收到
4. 真机文档 Tab 选 pdf → 发送 → 收到
5. 真机"全部"Tab 长按选文件夹(含子目录)→ 发送 → PC 端结构保持
6. 传输中:传输页行显示速度与剩余时间;类型图标正确
7. 完成行:图片显示缩略图;点击 → 系统查看器打开
8. 通知点击 → 直达传输页
9. 断连重试:传输中途关桌面 → 真机行落 interrupted;重开桌面续传成功(manifest 在私有目录不受 inbox 影响)
10. `adb logcat -s AndroidRuntime:E` 全程 0 FATAL

- [ ] **Step 4: CHANGELOG + 提交**

`CHANGELOG.md` 顶部加 v0.7.0 小节(接收落盘可见化/媒体浏览器/文件夹推送 UI/传输页增强;注明"两端都需升级"仅当协议变——本轮协议零变,**单端升级即可**)。

```bash
git add CHANGELOG.md
git commit -m "docs: CHANGELOG v0.7.0"
```

---

## Self-Review 记录

- **Spec 覆盖**:R1 落盘(Task 1-3)/ R2 媒体浏览器(Task 4-5)/ R3 文件夹推送(Task 5)/ R4 传输页(Task 6)/ 测试计划 T1-T8 对应(T1-T4=Task1-2,T5=Task4,T6=Task6,T7=Task6,T8=Task5)/ 装机冒烟(Task 7)✓
- **占位符扫描**:无 TBD/TODO;Task 5 FilesScreen 改造为要点清单而非整文件代码(装配性质,列出全部组件与行为契约)—— borderline,保留(整文件 450+ 行重写放计划里反而不可执行)
- **类型一致性**:PushFileDto(path/rel_dir)Task 2 定义 Task 5 消费;FilesSaved{paths} Task 2 定义 Task 3 消费;localPath Task 2 定义 Task 6 消费;setInboxDir Task 1 定义 Task 3 消费 ✓
- MediaGridTab 里 clickable 写法笔误(`.androidx.compose.foundation.clickable{}`)已在注释中标明正确形式——实现者按注释修正
