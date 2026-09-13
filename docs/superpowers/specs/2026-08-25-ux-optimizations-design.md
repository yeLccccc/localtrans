# UX 优化四项:长按菜单 / 双进度条 / 单击直拉 / hash 秒传 设计文档

日期:2026-08-25
版本目标:v0.10.0
前置:v0.9.2(40bedb6 修复传输页双任务/接收不显示后)

## 背景与目标

用户提出 4 个优化点:

1. **长按菜单**:Android 文件浏览(本机"全部"Tab + 远程页)长按单个文件直接弹出菜单(重命名/删除,远程页多一项下载),多选模式保留但入口迁移
2. **双进度条**:大文件传输任务单卡片内显示"已发送"(发送方视角)与"对方已收"(接收方确认视角)两条进度
3. **单击直拉**:远程页单击文件(非目录)直接发起拉取,入口从三步(长按→选中→底部栏)缩短为一步
4. **hash 秒传**:转发文件前先算整体 SHA-256,与对方收件箱已有文件比对,相同则复用本地文件零字节传输

### 现状事实(2026-08-25 核实)

- 文件页两处 `FileEntryCard`(FilesScreen.kt:243-254 本机 ALL Tab、337-349 远程页)长按=进入多选模式;多选后底部 `SelectionActionBar` 已有重命名/删除/下载(FilesScreen.kt:354-377)
- FFI 已有 `local_op`(FileOp::Rename/Delete/Mkdir,lib.rs:1828-1887)与 `share_op`(lib.rs:1654-1763)、`pull_files`(lib.rs:1288)——菜单只是新 UI 入口,FFI 零改动
- core `RecvAck`(protocol.rs:138)目前只有"整文件完成"一条(engine.rs:1226/2001 消费),**没有**逐块"对方已收字节"确认流——双进度条的 remote_done 需新增事件
- `OfferFile`(protocol.rs:198-202)字段为 name/size/rel_dir,无 hash
- core 已有 4MB 分块 SHA-256(manifest.rs:119-120,校验/续传用),无文件级 hash
- 大文件接收侧 `recv_push_large_file`(engine.rs:1133)有 `total_size` 与 Offer 声明的交叉校验(engine.rs:1190-1195)
- 发送方进度事件 `SourceSpeed`(engine.rs:379-397)已携带 bps/loss_ratio/rtt_ms/cwnd/streams,无 remote_done

## 模块 A:长按菜单 + 单击直拉(交互层)

### 组件

- 新建 `android/app/src/main/java/com/localtrans/app/ui/files/FileEntryMenuSheet.kt`:ModalBottomSheet 通用组件
- 修改 `FilesScreen.kt` 两处 `FileEntryCard` 调用点(本机 ALL Tab、远程页)

### 数据流

```
长按文件 → MenuSheet 弹出:
  本机页: [重命名] [删除] [选择多项]
  远程页: [下载到本机] [重命名] [删除] [选择多项]
菜单项回调 → 现有 ViewModel 函数(renameEntry / deleteEntries / pullFiles / toggleSelection)
单击远程文件(非目录)→ 直接 pullFiles(fp, shareId, listOf(path)) + Snackbar "已开始下载 X"
单击远程目录 → 进目录(现状不变)
"选择多项" → filesViewModel.toggleSelection(path) 进入现有多选模式,底部栏维持现状
```

FFI 零改动。删除入口复用现有确认门(core DeleteAsk + 壳层确认弹窗)。

### 错误处理

菜单操作失败走现有 `uiState.error` Snackbar 通道;单击直拉的误触兜底 = Snackbar 带"查看"动作跳传输页(可取消)。

### 测试

- ViewModel 单测(现有 Fake repo 注入菜单动作)
- MenuSheet 组件渲染测试(本机/远程两种菜单项集)

## 模块 B:双进度条(core + FFI + UI)

### 组件

- core:protocol.rs 新增 `RecvProgress { job_id, cumulative_bytes }` 控制消息;engine.rs 接收方发射点 + 发送方路由器消费点;`SourceSpeed` 事件扩 `remote_done: u64` 字段
- FFI:TransferDto 加 `remote_done: u64`;`handle_source_progress_event` 的 SourceSpeed 分支写入
- UI:TransfersScreen.kt push 卡片双进度条

### 数据流

```
接收方:每落盘一块(现 ChunkDone 发射点)累计字节 → 合并窗口(500ms 或每 32 块)
        → send_ctrl(RecvProgress { job_id, cumulative_bytes })
发送方路由器(现 RecvAck 处理点 engine.rs:2001 旁):
  收到 RecvProgress → 写 sender 任务状态(remote_done 累计值)
  → SourceSpeed 事件携带 remote_done 发出
FFI:handle_source_progress_event SourceSpeed 分支 → dto.remote_done = 事件值
UI:direction == "push" 的卡片画两条 LinearProgressIndicator:
  第一条 = done(已发送);第二条 = remote_done(对方已收)
  remote_done == 0 且 done > 0 时隐藏第二条(旧版对端无此功能,优雅降级)
```

接收方卡片(direction=rx/pull)不画第二条——本机进度即传输总进度。

**范围决策**:RecvProgress 仅在 push 方向接收侧发射(用户核心场景是"推大文件看对方收到多少")。
pull 方向(对方从我拉)的 source 行 remote_done 停 0,UI 走降级隐藏——功能无损,范围减半。

### 兼容与迁移

TransferDto 加字段 → uniFFI checksum 漂移 → Kotlin 绑定重生成 + AppException 手工补丁
(`val errorMessage` + `override val message`,既定流程)。PC 壳 TransferDto 同步加字段,
PC 前端显示算 backlog。旧版对端不回 RecvProgress → remote_done 停 0 → UI 降级隐藏。

### 测试

- core E2E:推送过程中 RecvProgress 累计值单调递增且终值 == total
- FFI 单测:SourceSpeed 写 remote_done 进 transfer 行
- UI 测试:push 卡片双条渲染条件(remote_done>0 显示,==0 隐藏)

## 模块 C:收件箱 hash 秒传(协议层)

### 组件

- core 新建 `crates/localtrans-core/src/transfer/dedup.rs`:收件箱索引 + 查询
- protocol.rs:OfferFile 加 `hash: Option<String>`
- engine.rs:push_files_inner 构造点算 hash;接收编排任务挂钩秒传判定;大文件 MetaResp 处挂第二判定点
- 双壳:状态文案加"秒传"一态

### 数据流

```
发送方 push_files_inner:
  构造 OfferFile 时算整体 SHA-256(流式)
  - 小文件(≤1MiB):同步算(毫秒级)
  - 大文件:后台并行算;offer 先发,hash 随 MetaResp 携带(接收方反向驱动时已就绪)
  → OfferFile { name, size, rel_dir, hash: Option<String> }

接收方(offer 编排任务 engine.rs:1941 spawn 点):
  小文件:OfferReq 携带 hash,接受后逐文件判定
  大文件:MetaResp 交叉校验点(engine.rs:1190 旁)判定(此时 hash 已就绪)
  判定逻辑:
    ①查收件箱索引(inbox_index.json:{hash → {path, size}},存于下载目录旁)
    ②命中且 size 一致且文件仍存在 → 本地复制(先试 hardlink,失败回退 copy)到目标位置
    ③发 ProgressEvent::Started + Done(秒传任务零块传输),累计进 JobDone 闭环
  未命中 → 正常传输;finalize 后把 (hash, final_path) 增量写入索引
```

### 关键决策

- **hash 计算不阻塞 offer**:大文件 hash 在 MetaResp 中补携带,offer 协议时序不变,
  大文件反向驱动(engine.rs:908-915 注册表竞态注释)不受影响
- **大小二次校验**:hash 命中后仍比对 size 与索引记录,不一致视为未命中(防索引脏数据)
- **同 offer 多文件**:逐个判定,"部分秒传 + 部分真传"混合是正常路径
- **安全**:hash 只做去重依据;索引含路径但不进日志(日志只打 hash 前 8 位,沿用明文不进日志规约);
  复制出的文件是本机已有文件,无新攻击面
- **失效清理**:启动时加载索引;命中时文件不存在 → 删该索引项;不做全量后台扫描(YAGNI)

### UI

秒传任务 StatusBadge 加"秒传"态;done 瞬间达成。双壳各改一处状态文案映射。

### 测试

- core E2E:同文件二次推送零块传输(断言无 FetchReq/复制成功且目标文件内容一致)
- core E2E:hash 未命中走正常传输且完成后索引写入
- 单测:索引损坏 → 降级为空表不炸;命中但 size 不符 → 视为未命中

## 收尾:版本与交付

- **版本 v0.10.0**:workspace Cargo.toml + src-tauri/tauri.conf.json + android/app/build.gradle.kts versionName 三处对齐(既定流程)
- **实现顺序**:A(纯 UI,零协议依赖)→ B(core+FFI+UI)→ C(协议层)→ 收尾
  B/C 都动 FFI 签名,Kotlin 绑定在 B、C 全部合完后只重生成一次
- **交付物**:PC zip + Android APK + relay tar.gz;dist 不含 data/ 目录(隐私红线)
- **明确不做**(YAGNI):全盘索引、PC 端双进度 UI、后台索引扫描、hash 计算内存映射优化

## 全局约束

- PSK 不进日志(服务端只打 sha256 前 8 位摘要)
- 配对码只在被连接方屏幕显示、协议/日志不含明文码
- 明文文件名/路径不进 tracing 日志(只打指纹/ID/数量);秒传日志只打 hash 前 8 位
- GitHub Release 只传 APK/zip;dist zip 不含 data/ 目录
- 提交信息:中文前缀 + 空行 + Co-Authored-By: Claude <noreply@anthropic.com>
- 测试:Rust per-crate `-- --test-threads=1`;FFI 测试端口占用批次(用户运行实例)已知非回归
