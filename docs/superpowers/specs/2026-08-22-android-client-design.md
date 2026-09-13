# LocalTrans Android 客户端设计(v0.6.0)

> 日期:2026-08-22
> 状态:已与用户逐节确认(架构/桥/UI/数据流/测试五节全部通过)
> 前置:v0.5.0 桌面端(推送体验完善)已发版;安卓开发环境(JDK17+SDK35+Gradle 8.10.2+LDPlayer)已部署于本机,参考 `C:/Users/<user>\Desktop\work\asr_app\docs\Android-App-开发方案.md`

---

## 0. 需求澄清结论(11 项全部确认)

| # | 问题 | 结论 |
|---|---|---|
| 1 | 「手机文件同步到本地电脑」语义 | **手动互传为主 + 相册备份**:与桌面版对等的手动推/拉,外加「相册自动备份」开关(新照片/视频自动推到电脑) |
| 2 | 文件浏览器+编辑搜索范围 | **本地+远程浏览,重命名/删除/新建**;搜索=当前目录按名过滤;「编辑」=系统「打开方式」交给其它 app,不在 app 内编辑内容 |
| 3 | 技术路线 | **Kotlin 原生(Jetpack Compose)+ 复用 Rust core**(交叉编译 .so) |
| 4 | 相册备份触发与目标 | **配对设备选一,存入其共享区** `LocalTransBackup/<设备名>/`;前台增量扫描(MediaStore 游标),不做唤醒式后台(杀 app 即停,重开续传) |
| 5 | 推送/接收确认对齐程度 | **对等对齐 v0.5.0 主体**:手机能推送+接收弹确认(倒计时);权限三档(每次询问默认/自动/拒)保留;Auto 档保留**一条**基础完成系统通知(与桌面「不无声塞文件」立场一致),首版不做的是通知长尾细节(另存顺延/进度常驻通知/通知内操作按钮) |
| 6 | FVP 范围 | **一条龙**:配对+互传+浏览器+备份,一次交付 v0.6.0 |
| 7 | JNI 桥方案 | **uniFFI 自动生成**(#[uniffi::export] facade + uniffi-bindgen 产 Kotlin 绑定) |
| 8 | 工程结构 | **同仓 monorepo**:localTrans 仓库新增 android/ + crates/localtrans-ffi/ |
| 9 | 发现机制 | **与桌面一致**:UDP 组播自动发现 + 中继远程设备(远程徽标),不做手动 IP 添加 |
| 10 | 传输后台保活 | **App 内后台**(切出 app 传输继续;被杀→断点续传恢复,与桌面体验一致);不上前台服务 |
| 11 | 分发/测试设备 | **APK 直接分发**(GitHub Release/dist,不上 Google Play);**模拟器为主**(LDPlayer/AVD+adb),发版前真机冒烟 |

---

## 1. 总体架构

```
┌─ Android App (Kotlin + Compose) ──────────────────┐
│  UI 层: Compose                                    │
│   设备页 / 文件浏览器 / 传输页 / 相册备份 / 设置页    │
│  ─────────────────────────────────────────────    │
│  ViewModel 层: 收集 uniFFI 回调 → StateFlow        │
│  ─────────────────────────────────────────────    │
│  JNI 桥: uniFFI 自动生成 Kotlin 绑定               │
│  ─────────────────────────────────────────────    │
│  localtrans-ffi (Rust, 新 crate)                   │
│   facade: #[uniffi::export] 包装 core Session      │
│   回调 trait → Kotlin interface (ForeignCallback)  │
│  ─────────────────────────────────────────────    │
│  localtrans-core (.so, 交叉编译 aarch64/armv7/x64) │
│   发现/配对/会话/传输引擎/中继客户端 —— 原样复用      │
└───────────────────────────────────────────────────┘
```

关键决策:

1. **core 零改动复用**——10611 行安全敏感代码(QUIC/TLS/配对/传输/中继)不重写,桌面/安卓协议天然一致,互通零特判。唯一例外:远程文件操作需 core 加 3 个协议消息(见 §2.3)。
2. **新增 `crates/localtrans-ffi`**——薄 facade(预计 500-800 行),把 core 异步 API 适配成 uniFFI 可导出的同步+回调风格。core 自身不引入 uniFFI 依赖,桌面端构建不受影响。
3. **`android/` Kotlin 工程**——Gradle Kotlin DSL(AGP 8.7.3/Kotlin 2.0.21/compileSdk 35/minSdk 26/JDK17),阿里云镜像,`cargo-ndk` 交叉编译。
4. **数据目录**——`context.getFilesDir()/localtrans/`(配置/身份/传输记录),语义对应桌面端 `data/`。身份=指纹,换手机=新设备需重新配对。
5. **回调桥**——core 事件流通过 uniFFI callback interface 推给 Kotlin,ViewModel 转 StateFlow 驱动 Compose。

## 2. JNI 桥(localtrans-ffi)

### 2.1 facade 形状

```rust
#[derive(uniffi::Object)]
pub struct LocalTransApp { /* 持有 core SessionManager + tokio runtime */ }

#[uniffi::export]
impl LocalTransApp {
    // 生命周期
    #[uniffi::constructor]
    pub fn new(data_dir: String, callback: Box<dyn LocalTransCallback>) -> Arc<Self>;
    pub fn start(&self);          // 启动发现+中继
    pub fn shutdown(&self);       // 优雅关闭(落盘进度,Goodbye 通知对端)

    // 发现/设备
    pub fn devices(&self) -> Vec<DeviceDto>;
    pub fn set_hidden(&self, hidden: bool);
    pub fn connect_device(&self, fingerprint: String);
    pub fn respond_consent(&self, fingerprint: String, accept: bool);
    pub fn submit_pairing_code(&self, fingerprint: String, code: String);

    // 文件浏览器
    pub fn list_local(&self, dir: String) -> Vec<FileEntry>;
    pub fn list_remote(&self, fingerprint: String, path: String) -> Vec<FileEntry>;
    pub fn rename_local / delete_local / mkdir_local(...);
    pub fn rename_remote / delete_remote / mkdir_remote(...);

    // 传输
    pub fn push_files(&self, fingerprint: String, paths: Vec<String>);
    pub fn pull_files(&self, fingerprint: String, remote_paths: Vec<String>);
    pub fn respond_offer(&self, job_id: u64, accept: bool);
    pub fn transfers(&self) -> Vec<TransferDto>;
    pub fn pause / resume / cancel / retry(job_id);

    // 相册备份(Kotlin 喂 MediaStore 路径,Rust 只管传)
    pub fn backup_push(&self, fingerprint: String, uris: Vec<String>);

    // 设置
    pub fn settings(&self) -> SettingsDto;
    pub fn save_settings(&self, s: SettingsDto);
    pub fn my_fingerprint(&self) -> String;
}

#[uniffi::export(callback_interface)]
pub trait LocalTransCallback: Send {
    fn on_event(&self, event: AppEvent);
}
// AppEvent = DeviceListChanged / ConsentRequested{fp,name} / PairingCodeShown{code}
//          / OfferRequested{job_id,peer,files,deadline_ms} / TransferUpdated{...}
//          / TransferDone{job_id,...} / RelayedDeviceSeen{...} ...
```

### 2.2 桥设计原则

1. **单一回调 trait + enum 事件**——新增事件只加 enum 变体,Kotlin `when(event)` 分发,桥表面积最小。
2. **同步方法 + 内部 tokio**——facade 持有 runtime,查询走 block_on/快照,结果异步回报走事件。与桌面壳层(Tauri 命令+事件)同构。
3. **panic 防护**——所有 export 方法 `catch_unwind` 包裹,panic 转 `AppError` 字符串返回,**绝不让 Rust panic 穿透 JNI**。
4. **进度节流**——进度类事件 Rust 侧 500ms 合并(与桌面壳层一致),避免高频回调打爆 UI。

### 2.3 core 改动:远程文件操作 3 消息(唯一实质性 core 改动)

现有 share 协议只有浏览(list)。新增:

- `ShareRename { path, new_name }` / `ShareDelete { path }` / `ShareMkdir { path }`
- 对端执行后回 `ShareOpResult { ok, error }`
- **向后兼容**:老版本收到新消息按未知消息忽略(serde 兼容路径与 v0.5.0 `reason` 字段同款);老版本收到操作请求返回「不支持」错误
- 桌面端 UI 本期不加这三个操作的入口,但桌面作为被操作方要能正确响应(消息处理进 core,壳层不用动)

### 2.4 构建链

- `cargo-ndk` 产 .so:arm64-v8a(主力)+ armeabi-v7a + x86_64(模拟器)
- Gradle 任务自动调 cargo + uniffi-bindgen;生成的 Kotlin 绑定提交进 git(`android/app/src/uniffi/`),保证不装 Rust 也能编 Kotlin;改 facade 后重新生成

## 3. App UI 结构(Kotlin + Compose)

单 Activity + Compose Navigation,底部导航 4 页:

### ① 设备页(首页)
- 设备卡片:名称+指纹缩写+在线状态+「远程」徽标+传输进行中角标
- 点卡片→连接(同意门+配对码,与桌面流程一致:对方屏幕出码,本机输码)
- 卡片操作:推送文件(SAF 文件选择器)/浏览对方文件/权限三档/断开
- 顶部:我的指纹+隐身模式开关

### ② 文件页(移动端独有)
- 位置切换:本机 / 已连接设备
- 本机侧根目录三入口:下载区 / 内部存储(受限) / App 数据目录
- 面包屑+返回上级(手势返回=上级)
- 每项:图标+名称+大小+时间;长按多选(底部操作栏:推送/重命名/删除)
- 右上角搜索:当前目录及子目录按名过滤(输入即过滤)
- 新建文件夹;点文件→系统「打开方式」

### ③ 传输页
- 任务列表:方向+文件名/数量+进度+速度+ETA+状态
- 状态流转与桌面一致:等待确认→传输中→完成/已拒绝/已超时/已中断
- 每任务:暂停/继续/取消;拒绝/超时带【重发】
- 接收确认:BottomSheet——文件数+总大小+倒计时,【接收】【拒绝】(首版不做另存顺延)
- 断点恢复:冷启动 interrupted→恢复横幅

### ④ 设置页
- 我的信息:本机名称(可改)+指纹
- 共享区:手机侧共享目录(默认 `下载区/LocalTransShare/`,可改)
- 中继:开关+服务器地址+PSK
- 连接安全:同意超时+推送确认超时(15-600s)
- 相册备份:开关+目标设备(已配对中选一)+照片/视频分开关
- 下载目录:默认 `Download/LocalTrans/`(SAF 授权)

### 技术要点
- ViewModel 持有进程级单例 `LocalTransApp`;回调→StateFlow→UI
- 存储/文件全部走 SAF,不用 READ_EXTERNAL_STORAGE;相册备份用 READ_MEDIA_IMAGES/VIDEO(API 33+)/旧权限回退
- 通知:仅 Auto 档接收完成一条系统通知(与桌面行为对齐)

## 4. 数据流与错误处理

### 4.1 传输数据流

```
Kotlin 选文件(SAF URI → FileResolver 解析/复制到缓存)
  → ffi.push_files(fp, paths)
  → core 传输引擎(QUIC 多路复用/滑动窗口/断点续传)
  → TransferUpdated 事件(500ms 节流)→ StateFlow → UI
  → 对端确认 OfferResp{accepted, reason}
  → TransferDone/Failed{fail_reason} → UI + (Auto 档)系统通知
```

SAF 返回 content:// URI 非文件路径——Kotlin 侧 FileResolver 负责解析真实路径或复制到缓存,不污染桥接口。

### 4.2 事件流(单一通道)

core 事件→ffi on_event(AppEvent)→Kotlin EventRouter→各 ViewModel StateFlow。冷启动用快照拉全量,之后增量事件。

### 4.3 相册备份流(前台)

```
App 前台 + 开关开 + 目标设备在线
  → MediaStore 增量扫描(游标 = 持久化的 max(media_id))
  → 新照片/视频 → ffi.backup_push(fp, paths) → 复用推送链路
  → 成功游标前移持久化;失败保留游标下次重扫
  → 存入对方共享区 LocalTransBackup/<设备名>/;设备名含目标平台非法字符时清洗(剔 `< > : " / \ | ? *` 与首尾空格点),清洗后为空则回退指纹前 8 位
```

### 4.4 错误处理分层

| 层 | 错误 | 处理 |
|---|---|---|
| JNI 桥 | facade panic | catch_unwind 全包裹→AppError,绝不穿透 JNI |
| core | 断连/拒绝/超时 | EngineError→AppError enum(NetworkOffline/PeerRejected/PeerTimeout/IoError...)→用户文案 |
| Kotlin | SAF 授权失效/文件被删 | FileResolver 捕获,toast 重新选择 |
| 传输中断 | 进程被杀/断网 | core 断点续传;冷启动恢复横幅 |
| 备份失败 | 设备离线/中继断 | 静默保留游标,设备回在线自动重试 |

### 4.5 安全约束(硬约束,延续现有规约)

- 配对码只在被连接方屏幕显示;`PairingCodeShown` 事件只发本机 UI;协议/日志永不含明文码
- 明文文件名/路径不进 tracing 日志(只打数量与 job_id)
- 中继数据面全程密文;身份=指纹,换机=新设备

## 5. 测试策略与交付物

### 5.1 四层测试

| 层 | 内容 | 工具 | 时机 |
|---|---|---|---|
| core 单测 | 3 个新消息:协议往返+权限拒绝+未知消息兼容(模拟老版本) | cargo test | 每次改 core |
| ffi 单测 | facade 方法映射/事件分发/错误转换;core test_support 起两实例对传 | cargo test | 每次改 facade |
| Kotlin 单测 | ViewModel 归约/EventRouter/FileResolver(SAF)/备份游标(纯 JVM) | JUnit+turbine | 每次构建 |
| 集成/E2E | 桌面 exe⇄模拟器 App:发现→配对→推拉→确认→断点续传→备份;adb screencap | adb+手动脚本 | 每任务+发版前 |

### 5.2 兼容性口径

- 手机 v0.6.0 ⇄ 桌面 v0.5.0:配对/推拉/确认/中继全部可用
- 远程文件操作(重命名/删除/新建)需对端 v0.6.0+;老桌面收到返回「不支持」,不炸不崩
- 桌面收到来自手机的推送,确认体验与桌面间推送一致

### 5.3 交付物

```
dist/localtrans-android-v0.6.0/
├── localtrans-v0.6.0-android.apk     # arm64-v8a + armeabi-v7a + x86_64
└── usage-android.md                   # 安装(未知来源)/配对/备份说明
```

版本 v0.6.0(桌面端不动,App 首版);CHANGELOG 加安卓端条目。

### 5.4 任务切分(实现计划时细化)

1. ffi crate 骨架+uniFFI 构建链跑通(hello 级,最大风险前置)
2. core:远程文件操作 3 消息(协议+引擎+测试)
3. ffi facade 完整 API+事件桥
4. Android 工程骨架(导航/主题/绑定注入)
5. 设备页+配对流程 UI
6. 传输页+接收确认 UI
7. 文件浏览器 UI(本地+远程)
8. 设置页+相册备份+下载区
9. E2E 互通验证(模拟器⇄桌面)+APK 打包

## 6. YAGNI 不做清单(本版明确排除)

- ❌ 完整文件夹双向同步(Syncthing 式,冲突/删除同步/扫描调度)
- ❌ app 内置文本编辑器(用系统打开方式替代)
- ❌ 前台服务/常驻通知保活(杀 app 即停,断点续传兜底)
- ❌ 另存顺延、除 Auto 完成通知外的系统通知细节
- ❌ Google Play 上架(个人 APK 分发)
- ❌ 手动 IP 添加设备入口
- ❌ iOS 端(uniFFI 理论可扩展,本期不做)
- ❌ 桌面端文件浏览器 UI(桌面本期不加远程文件操作入口)
- ❌ 二维码配对(可选 backlog)
