# localTrans 安全加固设计(攻击面审查修复)

日期:2026-08-24
状态:待用户审阅
来源:6 路攻击面审查(传输落盘/配对加密/中继/PC 桌面/Android/远程文件操作),Critical 与 High 发现均已由控制器逐条亲读源码复核确认。

## 威胁模型

**使用模型(产品定位,用户确认)**:share 共享发生在**不同用户的不同设备**之间——A 电脑把共享文件夹开放给 B 的手机浏览/拉取,配对双方通常不是同一个人的两台设备。因此"配对方"不是可完全信任的自己,而是设备另一端的**另一个人**(或被恶意软件接管的设备):越权操作、误删、恶意删除是一级威胁,不是边角情况。防护设计按"配对方半信任"取档。

- 攻击者与受害者在同一局域网,或位于受害者到中继服务器的网络路径上,或就是配对方本人(恶意或被接管)。
- 代码完全公开(开源),协议无模糊性优势。
- 不防御:受害者本机已被恶意软件完全控制(除密钥保护项);物理暴力接触。

## 修复分级

- **P0(开源发布阻塞项,5 项)**:推送落盘任意路径写入 ×2、远程文件操作零权限门+删除确认、relay 默认 PSK、批流 size 无上限、**移除信任即时生效(踢人踢不掉 bug)**。
- **P1(随下一版,6 项)**:relay 客户端证书校验、控制面长度上限、桌面 CSP+opener 收窄、Android allowBackup/FileProvider、私钥落盘加固、**Android 移除配对入口**。

---

# P0-1 推送落盘文件名净化(修复 C-1/C-2)

## 问题

两条推送落盘路径只净化 rel_dir(`sanitize_component`),不净化 file_name:

| 路径 | 漏洞点 |
|---|---|
| 小文件批流 | `engine.rs:2162`(name 仅 UTF-8 校验)→ `2197 join` → `2214 fs::write` |
| 大文件 finalize | `engine.rs:1134-1147`(MetaResp.file_name 全信)→ `PartWriter::finalize` `engine.rs:272-273 join` → rename |

Rust `Path::join` 遇绝对路径整体替换目标目录;`..\` 由 OS 解析穿越。Auto 档零交互写入启动目录 = RCE。变体:NTFS ADS(`evil.dll:hidden`)、Unicode RTL(U+202E)伪装扩展名、Windows 保留名、尾点/尾空格。

## 设计

新增统一净化函数 `sanitize_file_name(name: &str) -> Result<String, EngineError>`,放在 `transfer/mod.rs`(engine.rs 与测试共用),**拒绝制为主、剔除为辅**:

**拒绝(返回 `EngineError::Protocol("非法文件名: ...")`,仅记数量与 job_id,不记原串):**
1. 空串;
2. 含 `/` `\` `:` `*` `?` `"` `<` `>` `|` 任一字符(与 sanitize_component 同集,含 `:` 即挡 ADS 与盘符);
3. 等于 `.` 或 `..`;
4. Windows 保留名(大小写不敏感):`CON PRN AUX NUL COM1-9 LPT1-9`;
5. 以 `.` 或空格结尾(Windows 剥离语义陷阱);
6. 净化后 UTF-8 字节数 > 255。

**剔除(改写,不拒绝):**Unicode 控制字符(`U+0000-U+001F`、`U+007F`)与 bidi 控制符(`U+202A-U+202E`、`U+2066-U+2069`)——此类字符在真实文件名中几乎必为恶意,但剔除不破坏正常传输;剔除后若为空则按拒绝处理。

**三处接入点(纵深防御,每处都调):**
1. 批流入口:`recv_small_files_batched` 读出 file_name 后立即净化(engine.rs:2162 之后);
2. MetaResp 入口:`recv_push_large_file` 解构 file_name 后(engine.rs:1134 之后)——同时新增 **total_size 与 OfferFile.size 交叉校验**(两者不相等 → Protocol 错误,顺带堵"声明小 size 传输超大内容"的窗口);
3. 最终防线:`PartWriter::finalize` 内对 `manifest.file_name` 再校验一次(防未来新增调用点遗漏)。

**冲突改名路径同修:**批流冲突分支(engine.rs:2200-2212)当前用 `save_dir` 而非 `dest_dir` 拼新路径(子目录冲突文件会落回根目录),且新名含未净化 file_name——改为 `dest_dir.join(&new_name)`,new_name 基于已净化的名字构造。

## 兼容性

- 正常 Windows/Android 发送方:文件系统本身禁止上述字符,零影响。
- Linux/macOS 发送方含 `:` 等合法字符的文件名 → 传输失败,fail_reason 中文化为「文件名含目标系统不允许的字符」(localizeFailReason 映射 `非法文件名`)。
- 旧版本对端发送正常文件名 → 不受影响,无协议变更。

## 测试(TDD)

纯函数单测 `sanitize_file_name`(约 20 用例:绝对路径/`..`/ADS/保留名/尾点/RTL/正常名/emoji 名/Linux 合法名被拒/控制字符剔除),加两个集成测试:批流恶意名 job 落 failed 且不产生文件、MetaResp 恶意名同理。

---

# P0-2 远程文件操作权限门 + 删除确认(修复 C-3)

## 问题

`engine.rs:2005-2034`:`ShareRename`/`ShareDelete`/`ShareMkdir` 三个分支直接执行,不查 perms、不弹确认(对比同文件 ListReq 查 `perms.browse`、push 有 Offer 确认门)。被降权设备仍可 `remove_dir_all` 清空共享区,本机仅 debug 日志。

## 设计

### 权限门(三个分支统一)

入口加权限检查,**要求 `perms.browse`**(与 ListReq 对齐;下载权限不覆盖写操作,语义上"能浏览才能整理"):

```rust
// fail-closed:不在信任表(含已被移除) = 拒绝一切。
// 禁止照抄既有 ListReq 的 unwrap_or_default() —— Perms::default() 是
// browse=true/download=true,移除信任后的对端会话反而拿到全开默认权限。
let perms = ctx.trust.lock().await
    .get(&fingerprint).map(|p| p.perms.clone());
let allowed = perms.map_or(false, |p| p.browse);
if !allowed {
    let _ = sm.send_ctrl(&fingerprint, ControlMsg::ShareOpResult {
        ok: false, error: Some("无操作权限".into()),
    }).await;
    continue;
}
```

**同步修正既有 fail-open 点**:ListReq/MetaReq 等分支的 `unwrap_or_default()`(engine.rs:1474 等)一并改为 fail-closed(查不到 → 拒绝/空列表),否则"移除信任"对存续会话形同虚设——详见 P0-5。

与 ListReq 的差异:ListReq 无权限时回**空列表**(不暴露拒绝信息),Share* 回**明确失败**——操作类调用需要调用方知道失败原因,且会话已配对,不存在向未授权方泄露信息的问题。

### 删除确认门(用户已选定:每次删除弹确认)

`ShareDelete` 在权限门通过后**不再直接执行**,走本机 UI 确认:

**core 层**:新增 `DeleteAsk` 事件结构(仿 `OfferAsk`,engine.rs:406):

```rust
pub struct DeleteAsk {
    pub ask_id: u64,          // 单调计数器,壳层据此回话
    pub from: Fingerprint,
    pub share_id: String,
    pub name: String,         // 相对路径末段,仅本机 UI 展示(不进日志)
    pub is_dir: bool,
    pub entry_count: u64,     // 目录时的条目数(walk 一次统计,给用户判断依据)
    pub respond: oneshot::Sender<bool>,
    pub deadline_epoch_ms: i64,  // 复用 consent_timeout_secs 配置(默认 60s)
}
```

`spawn_rpc_router` 新增参数 `delete_ask_tx: mpsc::Sender<DeleteAsk>`。ShareDelete 分支:权限门 → 目录则统计条目 → 构造 DeleteAsk 发往壳层 → **spawn 独立子任务**等待 oneshot(带 deadline 超时),拿到 `true` 才执行 `op_delete`,否则回 `ShareOpResult{ok:false, error:"对方未确认删除"}`。超时 = 自动拒绝。

**关键并发约束**:RPC 路由器是单任务循环,若在循环体内同步等待用户响应,一个删除请求就能让后续所有 RPC(列目录/拉文件)卡到超时——恶意配对方可借此冻结浏览功能。因此等待必须移入 per-request 子任务;同时**待确认删除并发上限 8**,超出的直接拒绝(防弹窗轰炸)。

**fail-closed**:delete_ask 通道无消费者(壳层未接/已退出)时 oneshot 立刻失效 → 按拒绝处理,绝不静默放行。

**壳层(两端同构,复用 ask_rx 模式)**:
- 桌面(main.rs):新增 pending_deletes 表(仿 pending_offers);`delete-request` 事件推前端;新命令 `respond_delete(ask_id, allow)`;UI 全局模态:「{设备名} 请求删除共享文件夹中的 {name}(目录,{n} 项)/ 文件」+ 倒计时 + 允许/拒绝。
- Android(FFI lib.rs):ask 循环加 DeleteAsk 分支,emit `DeleteRequested` 事件;Kotlin AlertDialog(复用 OfferSheet 样式)中文化;`respondDelete(askId, allow)` FFI API。

**rename/mkdir 不加确认**(用户只选了删除加确认):rename 可逆、mkdir 单纯新增,权限门已够;delete 不可逆故单独设门。

**审计**:delete 执行成功从 `tracing::debug!` 升为 `tracing::info!`,带 share_id + 对端指纹缩写(不含文件名——遵守"明文文件名不进日志"规约)。

**新增 `manage` 权限位留待 P2**:需 UI 与 trusted_peers.json 迁移,不阻塞本次。

## 测试

- 权限门:browse=false 发 ShareDelete → ok=false 且未删;rename/mkdir 同理。
- 确认门:ask_rx 收到 DeleteAsk → respond(false) → ok=false 文件还在;respond(true) → 已删;超时(短 deadline)→ 自动拒绝。
- 并发上限:第 9 个待确认删除直接拒绝。
- 集成回归:确认等待期间 ListReq 仍正常响应(路由器未被阻塞)。

---

# P0-5 移除信任即时生效(踢人踢不掉 bug,多用户模型核心补救)

## 问题(用户模型审计发现的现有产品缺陷,非审查报告原始项)

多用户模型下,"发现配对方不可信 → 移除信任"是最核心的补救动作。当前链路有两处断裂:

1. **移除断会话缺失**:`remove_trusted`(commands.rs:1742)只删 trust 表记录并落盘,**不调 `sm.disconnect(&fp)`**(对比 reject_pairing commands.rs:172 有断开)。已建立的会话继续存活,对端可继续浏览/拉取/删除,直到自然超时。
2. **会话内存续期的权限 fail-open**:RPC 路由器权限查询用 `unwrap_or_default()`(engine.rs:1474、1510 等),对端被移除后查不到记录 → 回落 `Perms::default()` = **browse:true + download:true 全开**。叠加第 1 条:移除信任后,一台原本被降权的设备权限**反而升为全开**,直到会话断开。

两条叠加 = "踢人"按钮在多用户模型下完全失灵,且效果反向。

## 设计

三道修复,全部 fail-closed:

1. **移除即断开**:`remove_trusted` 在移除成功后调 `state.sm.disconnect(&fp).await`(与 reject_pairing 同模式)。双端一致:FFI 侧同补(P1-6 提供 Android 入口,但 core 行为本次先修)。
2. **权限查询 fail-closed**:RPC 路由器所有权限查询点(ListReq/MetaReq/SharesReq/ShareRename/ShareDelete/ShareMkdir)统一改为"查不到 → 拒绝",废弃 `unwrap_or_default()`。信任表查不到的会话本就不该存在(移除即断开),双保险兜底。
3. **`Perms::default()` 语义不动**(browse/download 默认true 是配对初始授权的合理默认),但**任何"不在信任表"路径一律不得回落 default** —— spec 代码示例已同步改为 `perms.map_or(false, |p| p.browse)`。

## 测试

- 已配对会话存续期间移除信任 → 会话立即断开(SessionDown 事件),后续 ListReq/ShareDelete 全拒。
- 移除后直接发 ListReq(不断开的重连边界)→ 空列表 + debug 拒绝日志,进程存活。
- 回归:正常配对会话权限不受影响(set_perms browse=false 后 ListReq 仍回空列表,既有行为)。

---

# P0-3 relay 配置缺失/弱 PSK 拒绝启动(修复 H-2)

## 问题

`relay/config.rs:37-45`:配置文件读不到时静默回退 `default_for_test()`(PSK=`"dev-psk"`,监听 0.0.0.0)。VPS 忘放配置文件 = 公网 9443 跑全世界皆知的 PSK。

## 设计

**原地改造 `load_or_default`**(全仓调用点仅 main.rs 一处,测试走 `for_test_with_port_offset` 不经过它):去掉默认值回退,重命名为 `load`,语义变严格:

- 文件不存在/解析失败 → `Err("配置文件 {path} 不存在或无效,拒绝启动。必须显式配置 psk。")`(进程非零退出);
- `psk.len() < 16` → `Err("psk 长度不足 16 字符,拒绝启动。建议 32+ 随机字节 hex。")`。

`default_for_test` / `for_test_with_port_offset` 保持不变(仅测试引用)。启动日志追加 PSK 指纹摘要(`sha256(psk) 前 8 hex`)供运维核对配置是否为预期值。

## 测试

单测:缺失文件 Err、短 PSK Err、合法配置 Ok、摘要格式。

---

# P0-4 批流 size 上限 + 流式收包(修复 H-3)

## 问题

`engine.rs:2184-2187`:批流声明的 size 直接 `vec![0u8; file_size as usize]`,声明 u64::MAX → 分配失败 abort,整进程崩溃,Auto 档零交互远程 DoS。

## 设计

两道防线:

1. **上限校验**:`file_size > SMALL_FILE_LIMIT`(1MiB,常量已存在于 engine.rs:881)→ `Err(EngineError::Protocol("批流文件超过小文件上限"))`,断开该任务流,job 落 failed,不影响进程;
2. **读包防截断**:`read_exact` 改为按 size 上限约束的读取——先校验后分配(防线 1 已保证 ≤1MiB,`vec![0u8; n]` 上限 1MiB 安全,无需再流式化)。

同时补 P1 同类项的廉价部分:**MetaResp total_size 与 OfferFile.size 交叉校验**已并入 P0-1 设计(同一次比较,挡"offer 声明 5MB、meta 声明 10TB"的 part.bin 扩展攻击)。磁盘余量检查(写前 `fs2::free_space`)记入 P1。

## 测试

单测难以直接构造批流(私有流格式),走集成:恶意对端发 size=8EB 的批流头 → 乙进程存活、job failed、fail_reason 中文映射。

---

# P1 项(随下一版,设计意向)

| # | 项 | 设计意向 | 主要位置 |
|---|---|---|---|
| P1-1 | relay 客户端证书校验 | 首连 TOFU:记录 relay 证书指纹到 config.json,后续连接强制比对,变更时 UI 告警;替代 `client.rs:511` NoVerify | relay/client.rs + store.rs |
| P1-2 | 控制面长度上限 | 长度前缀 >64KB 即断连,三处:relay control.rs:260、core client.rs:155、client.rs:472 | relay + core |
| P1-3 | 桌面 CSP + opener 收窄 | tauri.conf.json 设 `"csp": "default-src 'self'; script-src 'self'; connect-src 'self' ipc: http://ipc.localhost"`;opener scope 从 `**` 收到 `$DOWNLOAD/**` + 日志目录 | tauri.conf.json + capabilities |
| P1-4 | Android 加固 | AndroidManifest `allowBackup="false"`(+dataExtractionRules 排除 localtrans);file_paths.xml `Download/` → `Download/LocalTrans/` | android/app |
| P1-5 | 私钥落盘加固 | identity.key 写入后 0600(Unix)/Windows ACL;文档警示便携目录风险;DPAPI 加密与 keychain 迁移记 backlog | identity.rs |
| P1-6 | Android 移除配对入口 | FFI 补 `remove_trusted` API(移除即断开,P0-5 core 行为已修);设备页长按已配对卡片 →「移除配对」确认弹窗(中文化) | localtrans-ffi + DevicesScreen |

P2 backlog(记录不做):数据面 token 源地址绑定、指纹注册持有证明、发现广播设备表上限、`manage` 权限位、PSK 离线爆破进一步防御(P1-1 落地后攻击面已收窄)、disk free 检查、ShareOp 审计事件推送 UI。

## 明确不修(已评估的取舍)

- **远程 rename/mkdir 确认弹窗**:P0-2 仅 delete 设确认门——rename 破坏性低(改名前后对端可见)、mkdir 单纯新增。多用户模型下若需更严,P2 的 `manage` 权限位可整体收权(默认关闭 rename/delete/mkdir)。
- **发现广播"自证式签名"**:TLS 层指纹实拦(已验证),发现层仅提示;UI 已显示指纹缩写供人工核对,加 TrustStore 公钥扩展成本不成比例,记 backlog。
- **Android 公共 Download 目录 TOCTOU**:落盘位置是产品需求(用户要求文件落在可见区域),改为私有目录+MediaStore 属交互重构,记 backlog。

## 版本与发布

- **v0.8.2**(patch):P0 五项。协议无变更(全部是接收侧/服务端校验),旧对端发正常文件不受影响;接收侧升级即获保护。
- **v0.9.0**:P1 五项。
- 开源前检查单:P0 全绿 + CHANGELOG 安全小节 + `SECURITY.md`(威胁模型/上报渠道/已知取舍)。

## 测试总策略

- P0-1 纯函数 ~20 用例 + 2 集成;P0-2 3 集成;P0-3 4 单测;P0-4 1 集成;P0-5 3 集成(移除即断/查不到拒绝/降权回归)。
- 全量回归:core 既有测试(含配对/中继/断点续传 e2e);Android 侧 FFI 新增 DeleteRequested 事件与 respondDelete API,新增 ViewModel 用例,既有 49 用例回归(P0-2 涉及双端 UI)。
- 验收攻击演示(可选,验证修复有效性):本地起恶意对端发穿越文件名的 offer,确认 v0.8.2 接收侧拒绝且进程存活。
