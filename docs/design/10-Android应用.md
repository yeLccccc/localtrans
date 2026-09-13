# 10 Android 应用

> 源码:`crates/localtrans-ffi/`(lib.rs / state.rs / dto.rs / relay_state.rs)+ `android/`(Kotlin Compose 工程)。

## 1. 技术选型

- **uniffi 0.28**:Rust↔Kotlin 绑定(cdylib liblocaltrans_ffi.so,arm64-v8a + x86_64);callback interface 回传事件。
- Kotlin + Jetpack Compose(BOM 2024.10.01)+ material3 + navigation-compose + coroutines + Coil(+coil-video 视频帧)。
- minSdk 26 / target 35;versionCode 15 / versionName 0.11.0;com.localtrans.app。
- **无前台 Service**:传输在 Rust tokio runtime(2 worker)原生线程;UI 层 Repository suspend 封装,**阻塞 FFI 调用一律下放 IO 线程防 ANR**(T9)。

## 2. FFI 层设计(localtrans-ffi)

### 2.1 对象与状态

```rust
LocalTransApp {
    runtime: tokio::Runtime(2 worker),
    callback: Arc<Box<dyn LocalTransCallback>>,   // uniffi callback interface
    state: OnceLock<Arc<AppState>>,               // 初始化后读零锁
    init_lock: Mutex<()>,                          // 并发 start 互斥
}
```

FFI AppState 与 PC 壳同构,差异字段:`inbox_dir`(用户可见接收根,默认 data_dir,注入为 /sdcard/Download/LocalTrans)、`saved_files`(per-job 已存文件名累积,Done 时拼绝对路径发 FilesSaved)、`progress_throttle`(per-job 进度节流,FFI 特有)。

### 2.2 事件回调(AppEvent → Kotlin 密封类)

`LocalTransCallback::on_event(AppEvent)`:Hello / DevicesChanged / ConsentRequested / PairingCodeShown / PairingWaitConsent / PairingCodeEntry / PairingResult / SessionUp / SessionDown / TransferUpdated / TransferDone / **FilesSaved{job_id, paths}**(FFI 独有,绝对路径 → MediaScanner+查看跳转)/ OfferRequested / DeleteRequested / BackupProgress。

### 2.3 start() 初始化(与 PC 同构)

identity → config(**首装默认名"我的手机"**)→ trust → discovery → SessionManager+listener → 通道/ShareRegistry → transfers.json 恢复+GC → AppState → spawn_rpc_router → 五个事件泵(直接 callback.on_event)→ 会话事件桥(含回路 1/2 自愈)→ 发现 watch(DevicesChanged)→ 持久化循环 → spawn_relay。

### 2.4 FFI 全函数清单

| 函数 | 说明 |
|---|---|
| new(data_dir, callback) / hello / shutdown | 生命周期 |
| my_fingerprint | 指纹 hex(未启动返回 "not started") |
| inbox_dir / set_inbox_dir(dir) | 接收根(accept 时刻快照钉死,进行中任务不受影响) |
| start | 完整初始化 |
| devices | merge(本地+名册+connected+别名+信任) |
| set_hidden | 同 PC(翻转→重注册中继;★2026-08-30 定案:隐身语义修正——出站探测放行,双端同步) |
| local_ip / probe_addr(addr) | IP 自动补 47600;~~隐身时发现层门控跳过~~(★定案修正:出站探测不再被隐身门控);★local_ip 升级名片(全部地址打包,与 PC 同步) |
| connect_device(fp) | 本地→connect_pinned(**15s FFI 超时兜底**);名册→connect_peer+adopt |
| respond_consent / submit_pairing_code / cancel_wait | 配对三步 |
| settings / save_settings | SettingsDto(无 shares 字段——共享区管理仅 PC);改名 SetName+中继重启 |
| validate_relay / relay_status | 校验单一实现 / 拉模式状态(disabled/connecting/connected/error) |
| disconnect(fp) | 断会话 |
| push_files(fp, paths) / push_files_rel(fp, files) | 占位 + xfer_lock(60s) |
| pull_files(fp, share_id, remote_paths) | 落盘 inbox_dir(reg 传空 ShareRegistry) |
| respond_offer(job_id, accept) | accept 时 inbox_dir 快照钉进 saved_files root |
| respond_delete(ask_id, allow) | 删除确认门 |
| transfers / pause / resume / cancel | 表 + 两级控制(push_control→control_task) |
| transfer_remove(job_id) | 先 cancel → 删行+清 throttle+清 saved_files → 补发 TransferDone{removed} |
| transfers_clear_finished | 返回删除数 |
| retry_transfer(job_id) | pending_jobs 命中才续传 → start_pull_into 落 inbox |
| list_remote(fp, share_id, path) / remote_shares(fp) | ListReq/SharesReq,10s 超时(无自动重试,与 PC 异) |
| share_op(fp, share_id, op, path, new_name) | Rename/Delete/Mkdir → ShareOpResult |
| list_local(dir) / local_op(op, path, new_name) | 本地文件管理(fs metadata 跟随 symlink,FUSE 兼容) |
| backup_push(fp, paths, batch_tag) | rel_dir = LocalTransBackup/{设备名清洗}/{batch_tag}/;BackupProgress 事件 |

### 2.5 传输持久化(state.rs)

TransferRecord(serde,与 PC 同格式 job_id hex string,两端可互读);migrate(非终态→interrupted)/load/spawn_persist_loop(1s 脏标记,150 条封顶)。

## 3. Kotlin 应用结构

### 3.1 骨架

- **LocalTransBridge**(object):`context.filesDir/localtrans` 为 dataDir → 后台协程 start() → setInboxDir(Download/LocalTrans,失败回落私有目录);Callback → SharedFlow(256) + EventRouter;startError 暴露启动失败;pendingTab/pendingBrowseDeviceFp 两个深链槽。
- MainActivity:单 Activity(singleTop);init Bridge → 权限请求 → setContent{AppNav};onNewIntent 处理通知深链 open_tab=transfers。
- EventRouter:TransferDone→TransferNotifier 通知;FilesSaved→MediaScanNotifier.scanPaths(媒体库扫描)。
- data 层:DevicesRepo/FilesRepo/SettingsRepo/TransfersRepo(suspend 封装 FFI);media/(MediaRepo/MediaPickerViewModel/MediaScanNotifier)。

### 3.2 页面(AppNav 导航)

Devices(配对对话框 PairingDialogs + 接收确认 OfferSheet)/ Files(**四 Tab:相册/视频/文档/全部** + SendBar 统一发送栏 + UnifiedSelectionBar 多选)/ Transfers(速度/ETA/图标/查看深链/行级删除取消/清空已完成)/ Settings(设备名/隐身/中继状态行/接收目录)。长按媒体 → "发送到目标设备"。

### 3.3 权限(AndroidManifest)

INTERNET / ACCESS_NETWORK_STATE / ACCESS_WIFI_STATE / **CHANGE_WIFI_MULTICAST_STATE**(广播接收)/ READ_MEDIA_IMAGES/VIDEO/AUDIO / READ_EXTERNAL_STORAGE(≤32)/ WRITE_EXTERNAL_STORAGE(≤28)/ MANAGE_EXTERNAL_STORAGE / POST_NOTIFICATIONS。allowBackup=false;usesCleartextTraffic=false;FileProvider 查看跳转。

## 4. 与 PC 的功能差异矩阵

| 能力 | Android | PC |
|---|---|---|
| 相册备份推送(backup_push/BackupProgress) | ✅ | ❌ |
| 远程/本地文件操作 share_op/local_op/list_local | ✅ | 仅远程浏览,无操作 |
| 媒体浏览器四 Tab + 相册选择推送 | ✅ | 文件/文件夹选择(dialog 插件) |
| 接收目录 inbox_dir + FilesSaved + MediaScanner | ✅ | download_dir + 另存为任意绝对路径 |
| relay 状态 | 拉(relay_status) | 推(relay-state 事件) |
| 手动添加设备 | 只 IP(自动补端口) | IP:port + 5s 回查事件 |
| 文件夹拉取编排 / expand_local_paths | ❌ | ✅ |
| 防火墙管理 / 网络体检 | ❌(不适用) | ✅ |
| transfer_throttle 限速 / shares watchdog 事件 / 系统通知(Auto 档) | ❌ | ✅ |
| PSK 掩码回显 | ❌ | ✅ |
| 共享区管理(add_share) | ❌(SettingsDto 无 shares) | ✅ |
| progress_throttle 节流 | ✅(FFI 特有) | 动态频率聚合泵 |

## 5. 构建链

gradle `genUniffi` Exec task:`cargo run -p localtrans-ffi --bin uniffi-bindgen -- generate` → 生成 Kotlin 绑定拷入 `java/uniffi/`(生成物已入库);so 预编译入 jniLibs;`cargo ndk` 交叉编译。release:minify+shrinkResources+keystore 签名。
