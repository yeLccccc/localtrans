# 审计修复批次 v0.11.0 设计(全量 76 项)

日期:2026-08-26 | 输入:`docs/audit/2026-08-26-fullrepo-audit.md`(76 项:C2/S18/M31/L25)
执行:brainstorming → writing-plans → subagent-driven,四阶段,单版本 v0.11.0 收尾。

## 1. 目标与范围

把 2026-08-26 全仓库审计的全部 76 项发现在单个版本 v0.11.0 内修完:安全(信任链/服务防御/数据泄露)、稳定性(永久瘫痪/卡死/资源)、架构改造(路由器 spawn 化/FFI 锁/CSP)、Low 清扫,并以三端打包+装机双机验证收尾。

**不做什么**:不做新旧协议协商降级(硬切换);不改产品形态;不做与审计项无关的重构。

## 2. 全局约束

- **协议硬切换**:S1/S2/S3 改握手与消息格式,不做兼容分支;旧客户端/旧 relay 不保证互通,relay 与客户端(PC+Android)同步升级到 v0.11.0
- **版本**:三端对齐 0.11.0;Cargo.toml / src-tauri(两处)/ tauri.conf.json / android versionCode 15 + versionName "0.11.0"
- **提交规约**:中文前缀 + 空行 + `Co-Authored-By: Claude <noreply@anthropic.com>`;main 分支直接工作
- **测试规约**:Rust per-crate `cargo test -- --test-threads=1`;gradle 单测;每阶段末全量回归一次,阶段间不留半成品
- **安全红线(持续有效)**:PSK 不进日志(服务端只打 sha256 前 8 位);配对码只在被连接方屏幕显示,协议/日志不含明文码;明文文件名/路径/IP 不进 tracing 日志;dist 产物不含 data/
- **fail-closed**:所有验证类修复(钉扎/签名/成员校验/路径)失败即断/拒,不留降级路径
- **错误信息不回显内部绝对路径**(顺 A8)

## 3. 四阶段划分

| 阶段 | 内容 | 项数 |
|---|---|---|
| 一 | 安全+崩溃:C1/C2、S1-S9、数据泄露 M-A1~A8 | 28 |
| 二 | 稳定性:T1-T7、M-B1~B8、M-C1~C9 | 约 19 |
| 三 | 架构改造:路由器 spawn 化(T4/T5 根修)、FFI 全局锁重构、CSP/opener 收敛、秒传 oracle 完整方案 | 约 8 |
| 四 | Low 清扫 25 项 + 版本/三端打包/tag/装机验证 | 25+收尾 |

注:阶段一含 A3(CSP/opener)的收敛属安全级,阶段三的 CSP 条目为其延伸(前端命令面门禁),以阶段一完成基本盘为准。M-B/M-C 中与架构改造重叠的条目(T4/T5/M-B5)在阶段三根修,阶段二只做保守缓解。

## 4. 阶段一:安全+崩溃(28 项)

### 4.1 信任链三连(核心)

**S1 指纹钉扎**——把"预期指纹"贯通三条连接路径,复用 session.rs 现有 `expected` 比对,不加新密码学:
- 中继客户端侧:`virtual_ep::client_endpoint` 加 `expected_fp: [u8;32]` 参数,`connect_peer` 传入 punch 的 `target_fp`
- 中继服务端侧:握手完成后比对证书指纹 == `PunchNotif.from_fp`,不符即断
- 局域网侧:`SessionManager::connect(addr)` 加调用方传入 `expected_fp`(来自发现表签名指纹),ARP 欺骗下假证书握手失败

**S2 注册占有证明**——`Register` 消息加签:客户端用设备私钥(ed25519,identity.rs 已有 SigningKey)对 `(fingerprint || server_nonce)` 签名,完整 DER 证书随消息附带(服务端需从中取公钥并验指纹);服务端验"证书自签有效→证书指纹==声明 fingerprint→签名有效"三连,通过才进名册。server_nonce 在 TLS 握手后首发防重放。验签失败 → 记认证失败(入既有 IP 拉黑机制)+ 断连。

**S3 会话成员校验**——Punch 分配会话端口时 LeaseTable 登记 `(port → {fp_a, fp_b})`;KNOCK/DATA 校验 `src_fp ∈ 成员集`才进学习表;转发目标按成员差集精确寻址;非成员包丢弃+计数告警。成员登记同时作为 S6 端口回收锚点。

### 4.2 relay 服务防御

- **S4**:`conns` 锁内只做快照(clone Arc)即释放再推送;`push` 包 `timeout(3s)`,超时/失败断开该连接并清租约
- **S5**:双侧(服务端 control.rs + 客户端 client.rs)读帧 `len > 64*1024 → 断连`;`Register.name` 截断 256 字节
- **S6**:会话端口空闲 TTL = 2×lease_ttl,复用 5s reap 节拍;Punch 每指纹速率限制(如 10 次/分钟)
- **S7**:PSK 证明读包 `timeout(10s)`;Endpoint 层单 IP 并发上限(如 8)+ 全局连接上限(如 512);控制面 TransportConfig 去 keep-alive 反向保活
- **S8 ReplayGuard**(core/discovery.rs):先 `VerifyingKey::verify` 再动 nonce 表;容量满按 cutoff 重建(`filter(ts > cutoff)`)而非全清

### 4.3 客户端安全

- **S9 拉取路径穿越**:start_pull_dir 中远端 `rel` 的 parent 逐段过 `sanitize_component`,拒绝 `..`/绝对分量(对齐推送侧防线)
- **C1**:抽 `fn decode_fp32(s:&str) -> Result<[u8;32], AppException>`(hex decode + len==32),替换 lib.rs 四处 copy_from_slice(1117/1248/1370/1953);非 Result 导出函数体内零 panic
- **C2**:`start()` 改签名 `Result<(), AppException>`,内部 5 处 expect(304/370/405/418/421)全部 `?` 化;`new()` 的 runtime expect 同改;Kotlin 侧 start 失败走错误提示

### 4.4 数据泄露专项

| # | 修法 |
|---|---|
| A1 | Manifest `android:allowBackup="false"` + API31+ `dataExtractionRules` 排除 localtrans 目录 |
| A2 | LocalTransApp.kt:119 只打事件类型名;PairingCodeShown/TransferUpdated/FilesSaved 彻底脱敏 |
| A3 | tauri.conf.json 显式 CSP(`default-src 'self'; connect-src 'self' ipc: http://ipc.localhost`);opener `**` → 下载/日志目录;expand_local_paths/respond_offer.save_dir 路径门禁 |
| A4 | get_settings 的 relay_psk 回掩码(`****` + 尾 4 位),仅用户主动输入新值才下行;文档明示便携目录敏感性 |
| A5 | OfferResp 仅 `accepted==true` 时携带 skip 表 |
| A6 | relay NAT 漂移/KNOCK 日志 info→debug;relay-deploy.md 补日志保留策略 |
| A7 | 复制 IP 后 45s 定时清空剪贴板(Android + PC 双端) |
| A8 | api.ts 错误映射表,路径截断为主文件名;生产构建剥离 console.error |

## 5. 阶段二:稳定性

- **T1/T2 BufferPool RAII 化**:`acquire` 返回 guard(drop 自动 release),`permit.forget()` 模式废除;所有错误路径自动归还
- **T3 响应通道归还**:start_pull/ListReq 两处 send_ctrl 失败分支先 `return_inbound_resp_rx` 再返回错(照 engine.rs:2145 既有正确写法)
- **T4 保守缓解**:OfferReq Ask 档等待移入 spawn 子任务(照 ShareDelete 先例,注释已有"路由器循环绝不 await 用户响应")
- **T5 保守缓解**:`Manifest::build`/`op_delete`/`count_dir_entries`/`prune_missing` 包 `spawn_blocking`
- **T6**:`connect_device` 包 `timeout(15s)`;FFI 方法锁内只 clone Arc 句柄即释放,block_on 移出临界区
- **T7**:Kotlin handleSend 整树遍历移 `withContext(Dispatchers.IO)`
- **M-B1**:Session 加代次(generation),remove/门超时/断连前校验条目属本代
- **M-B2**:load_config 仅 NotFound 走"默认+落盘";其他读错误返回内存默认不回写;解析失败先备份旧文件再重置
- **M-B3**:relay 重连 spawn 前 abort 上一代 connect_task(generation 计数)
- **M-B4**:RPC 自动重试总时长压到 15s 内(6s+6s)
- **M-B5 保守缓解**:FFI list_remote/share_op 超时从 10s 降 6s(阶段三响应通道多路化根修)
- **M-B6 保守缓解**:毒锁防线——C1/C2 修复后全 FFI 面 `lock().unwrap_or_else(|p| p.into_inner())`(阶段三 OnceLock 重构)
- **M-B7**:进度泵 `transfer_get_mut().await.unwrap()` → `if let Some`
- **M-B8**:list_dir 硬上限 50k 条,超出报错
- **M-C1**:删除 legacy jobs 表(BitmapReq 已是死功能,全库无发送方,表无读者)——连带删除 BitmapReq 分支;若实现中发现隐藏依赖,降级为 RecvAck 处 remove 并在台账记录
- **M-C2**:.localtrans-parts 启动清理 N=7 天未更新目录;位图全真孤儿重跑 finalize
- **M-C3**:source_probe `idle_secs += 1`(按 tick 计数)
- **M-C4**:进度发送改 try_send(满则丢弃,进度事件可丢失)
- **M-C5**:三处持锁等通道改为先 remove/drop 守卫再 await(照 2145 写法)
- **M-C6**:FetchReq 传 `(chunk_len, offset)` 标量替代整份 Manifest clone
- **M-C7**:expand_local_paths/netsh/get_network_status 包 spawn_blocking + 结果缓存 5s
- **M-C8**:setFilter 加同值短路 + 移入 LaunchedEffect
- **M-C9**:validateRelay/respondDelete/respondOffer 三处主线程直调移 IO dispatcher

## 6. 阶段三:架构改造

- **路由器 spawn 化(根修 T4/T5/M-B5)**:重 IO(MetaReq 清单构建、OfferReq 编排、删除)全部移出路由器循环——spawn_blocking 执行 + 消息回发;响应通道从"全局单通道"改为按请求匹配(msg_id 路由的 mpsc/broadcast),消除 list/传输互斥
- **FFI 锁重构(根修 M-B6)**:`Mutex<Option<Arc<AppState>>>` 初始化后换 `OnceLock<Arc<AppState>>` 快照,读路径无锁;或锁内仅 clone Arc 即释放成为强制约定(审计检验点)
- **CSP/命令面(延伸 A3)**:逐命令审视暴露面,respond_offer.save_dir 校验位于用户授权目录、expand_local_paths 白名单化
- **秒传 oracle(完整方案,A5 延伸)**:skip 语义改为"接收方只回 bitmap,不回显 hash"或特性位协商,默认仅 trusted+accepted 场景启用

## 7. 阶段四:Low 清扫 + 收尾

25 项 Low 逐条修(见审计报告第六节清单,均为局部小改):cooldown 清理、ctrl send 超时、MetaResp 分页、share_watch spawn_blocking、InboxIndex 批量保存+唯一 tmp 名、PSK 摘要加长、PSK 恒时比较、hidden Punch 统一回包、带宽计数、通知方向过滤、搜索全量派生、saved_files 清理、桌面 push migrate、set_inbox_dir 竞态、PairingDialog 注销竞态、stat IO 下 IO 线程、死 ticker 删除、私有目录选项(文档明示)等。

**收尾**:版本三端对齐 v0.11.0 → 三端打包(PC exe / Android APK / relay win+ubuntu tar)→ 隐私核查(双 zip 0 data/)→ tag v0.11.0 → 装机双机验证清单(见 §9)→ 审计报告逐项标注修复状态。

## 8. 测试策略

**新增测试(红→绿)**:
- S1:中间人证书(非预期指纹)内层握手失败,双侧(中继/局域网)
- S2:无私钥抢注他人 fp 被拒 + 记认证失败;合法签名通过
- S3:非成员 KNOCK 丢弃;成员双包正常转发
- S4:慢读者(不读 uni 流)3s 后被断连,其他设备不受影响
- S5:0xFFFFFFFF 长度帧断连
- S6:空闲会话端口 TTL 后回收;Punch 速率限制触发
- S8:无效签名洪泛后合法重放仍被拒
- S9:ListResp 恶意 `../`/绝对路径被拒
- C1:空/短/长指纹调 push_files 返回错误不崩(Rust 侧单测)
- C2:端口占用时 start() 返回 Err 不 panic
- T1/T2:连续制造 N 次传输中断,池许可不耗尽(计数断言)
- T3:send_ctrl 失败后再次 pull 不报"通道被占用"
- M-B2:config.json 读失败(权限模拟)不回写
- A5:Deny 应答不携带 skip

**每阶段末全量回归**:cargo 三 crate `--test-threads=1` + gradle 单测 + `assembleRelease`;阶段四另加三端打包与隐私核查。

**装机双机验证(阶段四)**:
1. 旧版升级路径:v0.10.3 数据目录被 v0.11.0 正常读取
2. 中继互通:新 relay + 新双端,发现/配对/互传全走钉扎握手
3. 崩溃注入:空指纹 push 不崩;双开占端口 start() 报错不崩
4. 传输中断恢复:传输中断连 10 次,后续传输不再永久瘫痪
5. 数据泄露复查:adb logcat 无配对码/文件名;剪贴板 45s 后自清;设置页 PSK 显示掩码

## 9. 风险与回退

- 硬切换风险:升级窗口内旧新混连 → 中继功能不可用,直至三端同步升级;可接受(自用设备)
- 路由器 spawn 化动核心路径 → 阶段三独立全量回归 + e2e 全量;异常时该阶段可单独回退(commit 粒度)
- BufferPool RAII 化触碰所有传输路径 → 以 T1/T2 专项测试 + 全量回归兜底
- 每阶段独立提交序列,回退以阶段为单位

## 10. 交付物

- v0.11.0 代码(四阶段提交序列)
- `docs/audit/2026-08-26-fullrepo-audit.md` 逐项修复状态标注
- dist 三端产物 + tag v0.11.0
- CHANGELOG v0.11.0 条目
- 装机双机验证清单执行记录(台账)
