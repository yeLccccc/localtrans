# 安卓落盘可见化 + 移动端传输交互优化 设计文档

> 版本目标:v0.7.0 | 前置:v0.6.1 + 440b87e(所有文件访问授权)
> 状态:用户已批准设计,待落 spec 审阅

## 0. 背景与问题

v0.6.1 安卓端文件浏览器修复后仍遗留:

1. **接收文件落盘在私有目录**:`respond_offer` accept 时 `save_dir = state.dir`(filesDir/localtrans),用户在相册/文件管理器看不到收到的文件;config `download_dir` 默认值 `/system/bin/downloads` 在安卓不可写(从未被 FFI 使用,仅脏数据)
2. **选文件难**:LOCAL 页是平铺目录列表,图片视频文档混在一起,无缩略图无过滤
3. **手机侧主动推送弱**:只有 SAF 系统选择器入口,没有内建媒体浏览
4. **传输页信息密度低**:无速度/剩余时间,完成后无查看入口

主场景:**图片、文档、视频**的 PC ↔ 手机互传。

## 1. 需求(用户确认)

| # | 需求 | 决策 |
|---|---|---|
| R1 | 接收落盘位置 | 图片/视频进系统相册,文档进 Download/LocalTrans(方案 A:共享目录直写 + 媒体扫描) |
| R2 | 选文件交互 | App 内建媒体浏览器(相册/视频/文档三 Tab + 全部保留目录浏览) |
| R3 | 主动推送 | 手机可选文件**和文件夹**推送(push_files_rel 已有,补 UI 入口) |
| R4 | 传输页增强 | 速度 + 剩余时间 + 类型图标 + 完成后缩略图/查看跳转 |

**明确不做(本版)**:
- 落盘位置用户可配置(设置页只读展示,避免半成品配置项)
- App 内建图片全屏预览器(用 ACTION_VIEW 系统查看器)
- 接收目录按发件设备分子目录(沿用桌面端 LocalTransBackup/<设备名>/ 的模式评估过,本版统一 Download/LocalTrans/,简单直接)
- iOS/鸿蒙

## 2. 架构与数据流

### 2.1 R1 落盘可见化

```
LocalTransBridge.init(context)
  └─ FfiApp 构造后调 app.set_inbox_dir("/storage/emulated/0/Download/LocalTrans")
       (Kotlin 算路径: Environment.getExternalStoragePublicDirectory(DIRECTORY_DOWNLOADS)/LocalTrans)
       目录不存在则 Kotlin 侧先 mkdirs

接收链路(协议零变更):
  OfferSheet 接受 → respond_offer(accept=true)
    → save_dir = state.inbox_dir.clone()   ← 改(原 state.dir)
  → Rust 落盘 Download/LocalTrans/xxx.png
  → TransferDone{job_id, ok=true} 事件
    → EventRouter → MediaScanNotifier.notifySavedFiles(paths)
        ├─ 扩展名 ∈ 图片/视频 → MediaScannerConnection.scanFile()(即发即忘)
        └─ 其他 → 跳过(Download 目录文件管理器天然可见)

断点续传内部状态(manifest/parts)仍在私有目录 state.dir —— 不污染用户 Download。
```

**FFI 新增**:

```rust
// localtrans-ffi lib.rs
pub fn set_inbox_dir(&self, dir: String) -> Result<(), AppException> {
    // 写入 AppState.inbox_dir(默认 = state.dir,即不设置时行为不变)
}
```

- `AppState` 加 `inbox_dir: PathBuf` 字段;`respond_offer` 的 `save_dir`、`pull_files`/`retry_transfer` 的下载根(凡当前用 `state.dir` 做用户文件落点的)改用 `inbox_dir`
- `pending_jobs(&state.dir)`(断点 manifest 扫描)**不改**——内部状态归内部目录
- 桌面壳不受影响(不调 set_inbox_dir,行为同旧)

**文件名冲突**:core `finalize` 已有完整重名规避(`resolve_name_conflict`:目标存在则 `name(1).ext` 递增到 100,engine.rs:272),且 `finalize` 返回最终落盘 `PathBuf`——零改动,直接受益。

**媒体扫描触发面**:TransferDone(ok=true) 时,FFI 无法知道落盘的绝对路径列表(job 粒度)。两个方案:
- a) Kotlin 在 accept 时记下 job_id → inbox 目录快照,Done 后 diff 出新文件 —— 脆弱,弃
- b) **FFI 在 TransferDone 事件前发 `FilesSaved{job_id, paths: Vec<String>}` 新事件**(仅 FFI 层新增,core 协议不动),Kotlin 拿到精确路径再扫描 —— 采用。路径来源:接收链路里 finalize 的返回值(pull 链路在 FFI 落盘处收集)与小文件直接写入的返回路径

### 2.2 R2 内建媒体浏览器

```
文件页
├─ 一级 Tab: [本机] [远程]              ← 现有不动
└─ 本机 Tab 内部二级: [相册] [视频] [文档] [全部]
```

| Tab | 数据源 | 形态 |
|---|---|---|
| 相册 | MediaStore.Images(BUCKET 分组) | 网格缩略图 3 列,Coil 加载,长按多选 |
| 视频 | MediaStore.Video | 网格缩略图 + 时长角标,同上 |
| 文档 | FFI listLocal 扫 Download/Documents/DCIM(常见文档扩展名) | 列表 + 类型图标 + 大小/日期 |
| 全部 | FFI listLocal 目录浏览(现有,v0.6.1 刚修好) | 面包屑 + 列表,支持文件夹长按多选 |

- 新依赖:`io.coil-kt:coil-compose:2.7.0`(缩略图加载)
- MediaStore 查询走 `DATA` 列拿绝对路径(所有文件访问已授权)→ 推送直接给 Rust `push_files(paths)`
- 相册/视频 Tab 的 UI 状态(选中集、分组)独立于 FilesViewModel 的目录浏览状态——新建 `MediaPickerViewModel`,文件页按 Tab 切换 ViewModel

### 2.3 R3 文件夹推送

- "全部"Tab:长按文件夹 → 加入 selectedEntries(与文件同一套多选)
- 发送:`push_files_rel(fp, paths, rel_dirs)` —— core 已支持(桌面在用);FFI 现有 `push_files` 只传 paths,补一个 `push_files_rel` FFI 包装(Kotlin 传 `(path, relDir)` 列表)
- 相册/视频/文档 Tab:文件级,无文件夹概念

### 2.4 R4 传输页增强

| 项 | 实现 |
|---|---|
| 实时速度 | TransfersViewModel 记 (timestamp, done) 滑动窗(近 3s),差分得 B/s |
| 剩余时间 | (total - done) / speed,speed<1KB/s 显示 "--" |
| 类型图标 | 扩展名→图标映射(图/视频/音频/文档/压缩/文件夹/其他) |
| 完成行缩略图 | 图片/视频且行状态 done → Coil 加载 inbox 落盘文件小图(24dp 角标) |
| 点击查看 | done 行点击 → ACTION_VIEW(ACTION 内容由 TransferDto 新增 `local_path` 字段支撑,FFI 落盘时回填) |
| 通知跳转 | TransferNotifier 补 contentIntent → MainActivity(传输页深链:Intents extra `open_tab=transfers`,AppNav 读 extra 切 Tab) |

**TransferDto 扩展(FFI dto.rs)**:`local_path: Option<String>`(None 序列化为 null,Kotlin 侧可空)。首个落盘文件路径;文件夹任务为代表路径。

## 3. 组件与职责

| 单元 | 职责 | 依赖 |
|---|---|---|
| ffi `set_inbox_dir` + `inbox_dir` 状态 | 注入用户可见落盘根 | AppState |
| ffi `FilesSaved` 事件 + 落盘路径回填 | 精确通知 Kotlin 哪些文件入库 | 接收落盘链路 |
| `MediaScanNotifier`(Kotlin) | 收 FilesSaved → scanFile | MediaScannerConnection |
| `MediaPickerViewModel` + 三个 Tab UI | 媒体浏览与选择 | MediaStore/Coil |
| `SendBar` 组合件 | 已选汇总 + 设备底弹 + 发送 | push_files/push_files_rel |
| TransfersViewModel 速度窗 | 速度/ETA 计算 | 现有 TransferUpdated 流 |
| TransfersScreen 行改造 | 图标/缩略图/点击查看 | Coil + ACTION_VIEW |

## 4. 错误处理

| 场景 | 行为 |
|---|---|
| inbox 目录被删/不可写 | FFI 落盘 err → 现有 JobFailed 链路,行落"失败:…",不崩溃 |
| MediaStore 查询为空(权限被用户后续关闭) | 相册 Tab 显示"未授予权限"引导卡(复用 StoragePermissions.hasBrowsingPermissions) |
| MediaStore DATA 路径已失效(文件被删) | Coil 加载失败占位图;发送时 Rust 报 IO 错,行落失败 |
| scanFile 失败 | log 仅记录,不影响传输终态 |
| 推送文件夹不存在(浏览后被删) | push_files_rel Err,发送栏弹错误,选择保持 |

## 5. 测试计划

**Rust/FFI(cargo test -p localtrans-ffi)**
- T1 `set_inbox_dir` 后 respond_offer 的 save_dir 指向 inbox(用临时目录断言落盘位置)
- T2 不调 set_inbox_dir 时行为同旧(save_dir = state.dir)——兼容守护
- T3 TransferDone 前发 FilesSaved{paths} 且包含实际落盘路径
- T4 TransferDto.local_path 回填(首文件)与 None 序列化

**Kotlin 单测(gradle testDebugUnitTest)**
- T5 MediaPickerViewModel:Fake MediaRepo 下分组/选择/全选清空
- T6 速度窗:注入 (t, done) 序列断言 B/s 与 ETA 计算(纯函数抽 SpeedEstimator)
- T7 类型图标映射:扩展名→图标枚举全覆盖
- T8 SendBar 状态:选中数/大小汇总文案

**装机冒烟(模拟器 + 真机)**
- 桌面推图片到手机 → 相册 2s 内可见;推 pdf → Download/LocalTrans 可见
- 手机相册选 3 图 → 发 PC → 收到;文档 Tab 选 pdf 发送
- 全部 Tab 选文件夹(含子目录)发 PC → 结构保持
- 传输中看速度/ETA;完成后点行 → 系统查看器打开
- 通知点击 → 直达传输页

## 6. 交付物清单

- crates/localtrans-ffi: set_inbox_dir / inbox_dir / FilesSaved 事件 / local_path 字段 / push_files_rel 包装 / 相关测试
- android: MediaScanNotifier / MediaPickerViewModel + 相册视频文档 Tab / SendBar / TransfersScreen 增强 / TransferNotifier 跳转 / StoragePermissions 引导复用 / Coil 依赖 / 单测
- CHANGELOG v0.7.0
- 不动:core 协议、桌面壳、远程浏览

## 7. 风险

| 风险 | 缓解 |
|---|---|
| MediaStore DATA 列在新 Android 弃用趋势 | 目标版本可用;真机(16)已验证 FUSE 直读可行,DATA 返回真实路径;如未来失效,可退回 SAF requestId 流(记 backlog) |
| 相册大库(万张)首开慢 | MediaStore 分页(LIMIT/OFFSET 100)+ Coil 网格懒加载;分组聚合在 ContentResolver 查询层做 |
| Download/LocalTrans 与桌面 LocalTransBackup 语义并存 | 文档里写清:手机收 = Download/LocalTrans;PC 收 = 对方共享区 LocalTransBackup/<设备名>/,两者不冲突 |
| inbox 落盘与"所有文件访问"被撤销 | 落盘 err → 任务失败链路;设置页只读展示落盘位置引导用户检查授权 |
