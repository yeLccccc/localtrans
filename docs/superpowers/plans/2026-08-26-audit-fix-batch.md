# v0.11.0 审计修复批次实现计划(76 项,四阶段)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 docs/audit/2026-08-26-fullrepo-audit.md 的 76 项发现在单版本 v0.11.0 内全部修完,四阶段(安全+崩溃→稳定性→架构改造→Low 清扫+收尾),三端打包+装机验证收尾。

**Architecture:** 信任链修复靠"指纹贯通三条连接路径 + 注册签名 + 会话成员登记"三件套;稳定性修复以 RAII 化/通道归还/超时包装为主;架构改造把路由器重 IO spawn 化、FFI 全局锁快照化。协议硬切换,不做兼容分支。

**Tech Stack:** Rust(tokio/quinn/ed25519-dalek)、localtrans-relay、uniFFI、Tauri 2、Kotlin Compose、Vue3。

## Global Constraints

- 协议硬切换:改握手/消息格式不留兼容分支;旧端不保证互通
- 版本:三端对齐 0.11.0(Cargo.toml ×2、src-tauri/tauri.conf.json、android versionCode 15 + versionName "0.11.0")
- 提交:中文前缀 + 空行 + `Co-Authored-By: Claude <noreply@anthropic.com>`;main 直接工作
- 测试:Rust per-crate `cargo test -p <crate> -- --test-threads=1`;gradle `gradle -p android test`;每阶段末全量回归
- 安全红线:PSK 不进日志(服务端只打 sha256 前 8 位);配对码只在被连接方屏幕;明文文件名/路径/IP 不进日志;dist 不含 data/
- fail-closed:验证失败即断/拒
- 错误信息不回显内部绝对路径
- 环境命令:仓库根 `C:/Users/<user>/Desktop/work/localTrans`;bash;gradle 系统路径 `C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin/gradle`;JAVA_HOME 指向 jdk-17.0.20+8 内层

---

## 阶段一:安全+崩溃(28 项,Task 1-9)

### Task 1: S1 指纹钉扎——三条连接路径贯通预期指纹

**Files:**
- Modify: `crates/localtrans-core/src/relay/virtual_ep.rs:186-195`
- Modify: `crates/localtrans-core/src/relay/client.rs:344-411`(connect_peer/accept_peer/client_endpoint)
- Modify: `crates/localtrans-core/src/session.rs:787`(connect)、`session.rs:227-231`(expected 比对已有)
- Test: `crates/localtrans-core/tests/` 新增集成测试(放 relay e2e 或新建 `pinning_test.rs`)

**Interfaces:**
- Produces: `virtual_ep::client_endpoint(vudp, id, expected_fp: Fingerprint) -> Result<Endpoint, String>`;`RelayClient::connect_peer(target_fp, session_addr)` 内层握手带钉扎;`SessionManager::connect_pinned(addr, expected_fp: Fingerprint)`;原 `connect(addr)` 保留但仅在无指纹场景(测试)使用
- Consumes: session.rs 既有 `client_builder(Some(expected))` 验证逻辑

- [ ] **Step 1: 写失败测试**(中继路径:connect_peer 用中间人证书握手失败)

```rust
// tests/pinning_test.rs 核心断言(测试环境搭两套自签身份 A/B/M):
// A punch 后 connect_peer(target_fp=B_fp),M 的虚拟端点冒充 B → 握手 Err 且错误含"指纹"
#[tokio::test]
async fn relay_inner_handshake_rejects_wrong_fingerprint() {
    // ...搭 relay + A + B + 中间人 M(用 M 的 endpoint 冒充 B 的 session_addr)
    let err = a_client.connect_peer(b_fp, m_endpoint_addr).await.unwrap_err();
    assert!(err.contains("指纹") || err.to_lowercase().contains("fingerprint"));
}
```

- [ ] **Step 2:** `cargo test -p localtrans-core relay_inner_handshake_rejects_wrong_fingerprint -- --test-threads=1` → FAIL(当前握手成功)
- [ ] **Step 3: 实现**:`client_endpoint` 加 `expected_fp` 参数传给 `crate::session::client_builder(Some(expected_fp))`;`connect_peer` 签名加 `target_fp: Fingerprint` 并传入;`accept_peer` 握手后比对 `fingerprint_of(&conn.peer_identity())` == `PunchNotif.from_fp` 不符返回 Err 断连;局域网 `connect_pinned(addr, expected_fp)` 用 `client_builder(Some(expected_fp))`;FFI/Tauri 壳调用点改为传发现表指纹(grep `sm.connect(` 全部调用点更新)
- [ ] **Step 4:** 测试转绿 + `cargo test -p localtrans-core -- --test-threads=1` 回归
- [ ] **Step 5:** `git commit -m "fix(security): S1 内层握手指纹钉扎——中继/局域网三路径贯通预期指纹"`

### Task 2: S2 注册占有证明(Register 签名 + server_nonce)

**Files:**
- Modify: `crates/localtrans-core/src/relay/proto.rs:41`(Register 加字段)
- Modify: `crates/localtrans-core/src/relay/client.rs`(注册时签名)
- Modify: `crates/localtrans-relay/src/control.rs:195-279`(nonce 下发 + 三连验证)
- Modify: `crates/localtrans-core/src/identity.rs`(暴露 `sign(&self, msg: &[u8]) -> [u8;64]`)
- Test: relay e2e 新增用例

**Interfaces:**
- Produces: `Register { name, fingerprint, hidden, cert_der: Vec<u8>, nonce_sig: [u8;64] }`;控制面首条消息新增 `ServerNonce([u8;32])`(TLS 握手后服务端先发);`Identity::sign`
- Consumes: Task 1 无依赖

- [ ] **Step 1: 失败测试**:`抢注他人 fp(用自己私钥签)被拒 + 记认证失败;合法签名注册通过`(e2e 内两个断言用例)
- [ ] **Step 2:** 跑 → FAIL
- [ ] **Step 3: 实现**:proto 加字段;identity.rs 加 `sign`;client 在收到 ServerNonce 后构造签名(`sign(fp || nonce)`)随 Register 发;control.rs 验:证书自签解析→`fingerprint_of(cert)==声明 fp`→`VerifyingKey::from(cert).verify(fp||nonce, sig)`→全过才 `leases.alloc` + `conns.insert`,任一失败走既有 `record_auth_failure` + 断连;nonce 每连接一次性
- [ ] **Step 4:** 转绿 + relay `cargo test -p localtrans-relay -- --test-threads=1` 回归
- [ ] **Step 5:** `git commit -m "fix(security): S2 Register 携带私钥签名,指纹抢注被拒"`

### Task 3: S3 会话成员校验 + S6 端口 TTL/速率限制

**Files:**
- Modify: `crates/localtrans-relay/src/lease.rs`(session_members: `HashMap<u16, (Fp, Fp)>`;reap 顺带 TTL 回收)
- Modify: `crates/localtrans-relay/src/control.rs:321-376`(Punch 登记成员 + 每 fp 速率限制)
- Modify: `crates/localtrans-relay/src/data_plane.rs:141-209`(KNOCK/DATA 成员校验 + 精确寻址)
- Test: relay e2e

- [ ] **Step 1: 失败测试**:`非成员 fp 的 KNOCK 被丢弃(不进学习表);空闲会话端口 2×lease_ttl 后回收;同 fp 连发 Punch 超 10 次/分被限`
- [ ] **Step 2:** FAIL
- [ ] **Step 3: 实现**:Punch 分配端口时写 `session_members.insert(port,(a,b))`;KNOCK/DATA `src_fp` 不在成员集 → 丢包+`tracing::warn!`(打 fp 前 8 hex)+计数;转发目标 = 成员差集;reap_expired 清 `last_activity > 2*lease_ttl` 的端口与成员;Punch 前置 `punch_rate: HashMap<Fp, Vec<Instant>>` 滑窗 10/min
- [ ] **Step 4:** 转绿 + 回归
- [ ] **Step 5:** `git commit -m "fix(security): S3+S6 会话成员校验与端口 TTL——KNOCK 注入被拒,端口可回收"`

### Task 4: S4 持锁推送死锁 + S5 帧上限 + S7 认证超时/连接上限

**Files:**
- Modify: `crates/localtrans-relay/src/control.rs:260-264(帧上限),182-192(拉黑前置检查仍需握手后,补 Endpoint 层单 IP/全局上限),195-197(证明读超时),304-316,421-462(快照+超时推送)`
- Modify: `crates/localtrans-core/src/relay/client.rs:166-171,483-488`(客户端侧帧上限)
- Modify: `crates/localtrans-relay/src/main.rs`(Endpoint 连接上限配置)
- Test: relay e2e

- [ ] **Step 1: 失败测试**:`0xFFFFFFFF 长度帧断连;慢读者(不读 uni 流)3s 被断连且其他连接不受影响`
- [ ] **Step 2:** FAIL
- [ ] **Step 3: 实现**:双侧 `if len > 64*1024 { return Err(断连) }`;`Register.name` truncate 256;广播/推送:锁内 `Vec<(Fp, Arc<Conn>)>` 快照→drop 锁→逐个 `timeout(3s, push)`,失败 `conn.close()` + 清租约;`accept_bi` 等 PSK 证明包 `timeout(10s)`;`Endpoint` concurrent 连接上限(lib 侧用 `conns.len() >= 512` 拒新)+ 单 IP(同 SocketIp 计数 ≥8 拒);控制面 TransportConfig `keep_alive_interval(None)`
- [ ] **Step 4:** 转绿 + 回归
- [ ] **Step 5:** `git commit -m "fix(security): S4/S5/S7 relay 服务防御——持锁推送超时、帧上限、认证超时与连接上限"`

### Task 5: S8 ReplayGuard 验签前置 + S9 拉取路径净化

**Files:**
- Modify: `crates/localtrans-core/src/discovery.rs:76-79,125-155`
- Modify: `crates/localtrans-core/src/transfer/engine.rs:2708-2717`
- Test: `crates/localtrans-core/tests/`(discovery 单测 + 拉取净化单测)

- [ ] **Step 1: 失败测试**:discovery:`无效签名包不消耗 nonce 槽(容量满重建后合法重放仍被拒的时间窗语义用单测模拟)`;engine:`sanitize_rel_path("../evil/x") -> Err`、绝对分量被拒(纯函数抽出后直测)
- [ ] **Step 2:** FAIL
- [ ] **Step 3: 实现**:discovery.rs 调序——`verify` 先行,失败早退;容量满 `filter(|(_,ts)| *ts > cutoff)` 重建;engine.rs 抽 `fn sanitize_rel_parent(rel:&str) -> Result<PathBuf, EngineError>`,start_pull_dir 的 parent 逐段 `sanitize_component`(复用既有函数),拒绝 `..`/绝对分量
- [ ] **Step 4:** 转绿 + core 回归
- [ ] **Step 5:** `git commit -m "fix(security): S8/S9 ReplayGuard 验签前置重建、拉取侧路径穿越净化"`

### Task 6: C1+C2 FFI 零 panic 化(decode_fp32 + start() Result 化)

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs:304,370-371,405,418,421(5 处 expect),1117,1248,1370,1953(4 处 copy_from_slice)`
- Modify: `android/app/build.gradle.kts`(无关)——Kotlin 调用点:`android/.../LocalTransApp.kt`(start 失败处理)
- Test: `crates/localtrans-ffi/src/lib.rs` tests 模块

**Interfaces:**
- Produces: `fn decode_fp32(s: &str) -> Result<[u8;32], AppException>`(自由函数);`LocalTransApp::start(&self) -> Result<(), AppException>`;uniFFI 绑定重生成(绝对路径 out-dir 流程照 build.gradle.kts genUniffi 任务)+ errorMessage 手工补丁重做

- [ ] **Step 1: 失败测试**:`decode_fp32("")`/短 hex/超长 hex → Err;`start()` 在端口被占场景返回 Err(测试内绑同端口两次断言第二次 Err 不 panic)`
- [ ] **Step 2:** FAIL
- [ ] **Step 3: 实现**:抽 `decode_fp32` 替换四处;`start()` 签名改 Result,内部 expect 全 `?` 映射 AppException::Internal/Io;`new()` 的 runtime expect 改返回 Err;Kotlin `LocalTransApp` 捕获 start 错误走 UI 提示(不崩);so 重编 + 绑定重生成 + errorMessage 补丁(构造参数去 val + 类体 val errorMessage + override val message)
- [ ] **Step 4:** `cargo test -p localtrans-ffi -- --test-threads=1` 转绿;gradle 编译过
- [ ] **Step 5:** `git commit -m "fix(critical): C1/C2 FFI 零 panic 化——指纹解码校验与 start() Result 化,Android abort 根除"`

### Task 7: A1+A2 allowBackup + logcat 脱敏

**Files:**
- Modify: `android/app/src/main/AndroidManifest.xml`
- Create: `android/app/src/main/res/xml/data_extraction_rules.xml`、`backup_rules.xml`
- Modify: `android/app/src/main/java/com/localtrans/app/LocalTransApp.kt:119`
- Test: 装机验证(阶段四清单)+ grep 断言

- [ ] **Step 1:** Manifest `<application>` 加 `android:allowBackup="false" android:dataExtractionRules="@xml/data_extraction_rules" android:fullBackupContent="@xml/backup_rules"`;两 xml 均排除 localtrans 域
- [ ] **Step 2:** `LocalTransApp.kt:119` 改 `Log.d(TAG, "event: ${event::class.simpleName}")`——只打类型名;grep 确认全 Kotlin 无 `Log.*("$event")`/配对码/文件名直打
- [ ] **Step 3:** `gradle -p android assembleRelease` 编译过 + grep 断言零命中
- [ ] **Step 4:** `git commit -m "fix(privacy): A1/A2 allowBackup 关闭与 logcat 事件脱敏"`

### Task 8: A3-A8 数据泄露前端/壳层(除 A5 协议部分)

**Files:**
- Modify: `src-tauri/tauri.conf.json:10-12`(CSP)、`src-tauri/capabilities/default.json`(opener 收敛)
- Modify: `src-tauri/src/commands.rs:1633-1656`(A4 PSK 掩码)、`1117-1178`(expand_local_paths 门禁)
- Modify: `ui/src/api.ts:47`(A8 错误映射)、`ui/src/pages/*.vue` 错误展示点
- Modify: `android/.../ui/devices/DevicesScreen.kt:202-209` + `ui/src/pages/Devices.vue:181-189`(A7 剪贴板 45s 清理)
- Modify: `crates/localtrans-relay/src/data_plane.rs:180-183` + `dist/.../relay-deploy.md`(A6 日志降级+文档)

- [ ] **Step 1:** CSP 设 `"csp": "default-src 'self'; connect-src 'self' ipc: http://ipc.localhost; img-src 'self' data:"`;opener allow 从 `**` 改 `["$DOWNLOAD/**", "$LOGDIR/**"]`;`expand_local_paths` 加根白名单(用户家目录/盘符根二级以内,拒绝系统目录);PSK 回显掩码:`ConfigDto.relay_psk` 返回 `format!("****{}", &psk[psk.len().saturating_sub(4)..])`(psk.len()<8 时全掩),save_settings 对 `****` 开头的 psk 不覆盖原值;api.ts 错误映射函数 `friendlyError(cmd, raw)`(正则截断路径为主文件名);剪贴板:复制后 `LaunchedEffect` delay 45s 置空(双端);relay 日志 info→debug + deploy 文档补日志轮转节
- [ ] **Step 2:** `cargo test -p localtrans-core -- --test-threads=1` + `cargo build` PC 壳编译过;gradle 编译过;grep 断言:tauri.conf 无 `**` opener、Kotlin/Vue 无剪贴板裸复制残留
- [ ] **Step 3:** `git commit -m "fix(privacy): A3/A4/A6/A7/A8 CSP 收敛、PSK 掩码、日志降级、剪贴板自清、错误映射"`

### Task 9: A5 秒传 oracle + 阶段一回归

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs:1969-1986`
- Test: core e2e 既有秒传用例调整断言

- [ ] **Step 1: 失败测试**:`OfferReq 被 Deny 时 OfferResp.skip 为空`
- [ ] **Step 2:** FAIL
- [ ] **Step 3:** `let skip = if accepted { 计算 skip } else { Vec::new() };`
- [ ] **Step 4:** 全量回归:cargo 三 crate `--test-threads=1` + gradle test;台账记阶段一完成
- [ ] **Step 5:** `git commit -m "fix(privacy): A5 秒传 skip 仅在接受时回传——拒绝不再泄漏文件持有信息"`

## 阶段二:稳定性(19 项,Task 10-15)

### Task 10: T1/T2 BufferPool RAII 化

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs:320-345(BufferPool),1537-1561(run_sender),1484-1509(run_receiver_windowed)`
- Test: engine 模块测试 + 新增中断测试

**Interfaces:**
- Produces: `BufferPool::acquire() -> BufferGuard`(guard 持 `&Arc<Inner>` + BytesMut,Drop 自动 release;release 兼容旧调用点逐步迁移);`struct BufferGuard`

- [ ] **Step 1: 失败测试**:`连续 40 次 open_uni 失败路径后 pool.available_permits() 仍 == 初始值`(暴露 permit 泄漏)
- [ ] **Step 2:** FAIL
- [ ] **Step 3: 实现**:新增 `BufferGuard`;`acquire` 返回 guard;run_sender/run_receiver_windowed 全路径用 guard(错误路径自动归还);`release(buf)` 保留给既有手动点;`available_permits()` 测试钩子 `#[cfg(test)]` 或 pub
- [ ] **Step 4:** 转绿 + core 回归(重点 push/pull e2e)
- [ ] **Step 5:** `git commit -m "fix(stability): T1/T2 BufferPool 许可 RAII 化——错误路径自动归还,断传不再耗尽共享池"`

### Task 11: T3 响应通道归还 + M-B4 重试窗口

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs:556-564,2608-2614`
- Modify: `src-tauri/src/commands.rs:250-354`(10s+10s → 6s+6s)

- [ ] **Step 1: 失败测试**:`send_ctrl 失败(MetaReq 对端不存在)后,再次 start_pull 不报"通道已被占用"——用 mock/断连对端模拟`
- [ ] **Step 2:** FAIL
- [ ] **Step 3:** 两处失败分支先 `sm.return_inbound_resp_rx(resp_rx).await` 再 `return Err`(照 engine.rs:2145 既有写法);桌面壳两处 RPC timeout 改 6s+6s
- [ ] **Step 4:** 转绿 + 回归;`git commit -m "fix(stability): T3/M-B4 响应通道失败归还与重试窗口压缩"`

### Task 12: T4/T5 路由器保守缓解 + M-C5/M-C7 壳层 IO

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs:1934-1956(Ask spawn 化),1693-1699(Manifest spawn_blocking),2292(count_dir),1971(prune)`
- Modify: `crates/localtrans-core/src/share.rs:238-245(op_delete spawn_blocking)`
- Modify: `src-tauri/src/commands.rs:1117-1178(expand_local_paths spawn_blocking),2073-2084(get_network_status 缓存 5s)`、`src-tauri/src/firewall.rs:82-84(spawn_blocking 包裹)`

- [ ] **Step 1:** 实现(行为等价重构,既有 e2e 作回归门):OfferReq Ask 等待移 `tokio::spawn`(照 2324 ShareDelete 先例,应答回路由器续发 OfferResp);`Manifest::build`/`op_delete`/`count_dir_entries`/`prune_missing` 包 `tokio::task::spawn_blocking(...).await`;expand_local_paths/netsh 同
- [ ] **Step 2:** `cargo test -p localtrans-core -- --test-threads=1` + e2e 全绿;PC 壳 `cargo build` 过
- [ ] **Step 3:** `git commit -m "fix(stability): T4/T5/M-C5/M-C7 路由器重 IO spawn 化与壳层阻塞 IO 下放"`

### Task 13: T6/M-B6 FFI connect 超时 + 锁外 block_on + 毒锁防线

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs:840-886,1707-1761,1817-1877`(全部 `state.lock().unwrap()` 调用点)
- Modify: `crates/localtrans-core/src/session.rs`(connect 包 timeout 由 FFI 侧做)

- [ ] **Step 1:** `connect_device` 的 `sm.connect` 包 `tokio::time::timeout(Duration::from_secs(15), ...)`;全部 FFI 方法改为:锁内只 `let app = state_guard.clone();` drop(guard) 后 block_on(模式:`let app = { self.state.lock().unwrap_or_else(|p| p.into_inner()).clone() };`);全部 `lock().unwrap()` → `lock().unwrap_or_else(|p| p.into_inner())`
- [ ] **Step 2:** `cargo test -p localtrans-ffi -- --test-threads=1`;so 重编 + 绑定重生成 + errorMessage 补丁;gradle 编译
- [ ] **Step 3:** `git commit -m "fix(stability): T6/M-B6 FFI 连接超时、锁外执行与毒锁防线"`

### Task 14: T7/M-C8/M-C9 + M-B1/M-B2/M-B3 Android 与会话状态机

**Files:**
- Modify: `android/.../ui/files/FilesScreen.kt:653-679(handleSend IO 化),181(setFilter)`
- Modify: `android/.../ui/settings/SettingsViewModel.kt:161`、`ui/nav/AppNav.kt:186-204`(主线程 FFI 直调移 IO)
- Modify: `crates/localtrans-core/src/session.rs:1128-1155,1513`(Session generation)
- Modify: `crates/localtrans-core/src/store.rs:66-77`(load_config 读失败不回写)
- Modify: `crates/localtrans-ffi/src/relay_state.rs:88-92` + `src-tauri/src/commands.rs:1979-1983`(connect_task generation abort)
- Test: store 单测 + session 竞态测试

- [ ] **Step 1: 失败测试**:`load_config 在 read 错误(非 NotFound)时不写盘`(单测注入不可读路径);`旧代 ctrl_loop 退出不删新一代 session`(session 竞态测试)
- [ ] **Step 2:** FAIL
- [ ] **Step 3:** store.rs:仅 `NotFound` 落盘默认,其余 Err 返回内存默认不回写,解析失败先 `rename(path, path.bak)` 再重置;session.rs:Session 加 `generation: u64`(原子递增),insert 覆盖时旧代+1,ctrl_loop remove 前比对代次,门超时任务校验代次;relay_state/commands:spawn 前 `if let Some(t) = old_task { t.abort() }` + generation 计数写回校验;Kotlin 三处改动照 Task 内描述
- [ ] **Step 4:** 转绿 + gradle 编译;`git commit -m "fix(stability): T7/M-B1/B2/B3/C8/C9 ANR 缓解与会话/配置/中继竞态修复"`

### Task 15: M-B7/M-B8/M-C1~C6/M-C9 资源与性能批

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs:2494(B7 unwrap→if let)`
- Modify: `crates/localtrans-core/src/share.rs:143-206(B8 上限 50k)`
- Modify: `crates/localtrans-core/src/transfer/engine.rs:1593,1747(C1 删 jobs 表),610-621+mod.rs(C2 parts GC 7 天),source_probe.rs:59-62(C3),1484-1490(C4 try_send),817-829+sender_state.rs:93-104+1835-1844(C5 先 remove 后 send),1879-1886(C6 传标量)`
- Test: 对应模块单测

- [ ] **Step 1: 失败测试**:`source_probe idle 计数按 tick 累加`(单测);`list_dir 超上限报错`;`parts 目录 7 天前时间戳被启动清理`(时间注入)
- [ ] **Step 2:** FAIL
- [ ] **Step 3:** 逐项实现(行号锚点如上;C1 先 grep `jobs.write()`/`BitmapReq` 确认无隐藏读者再删,发现依赖则降级 RecvAck remove 并记台账)
- [ ] **Step 4:** 全量回归:cargo 三 crate + gradle;台账记阶段二完成
- [ ] **Step 5:** `git commit -m "fix(stability): 资源与性能批次——jobs 表/parts GC/探针计数/进度背压/锁序/Manifest 拷贝"`

## 阶段三:架构改造(8 项,Task 16-18)

### Task 16: 响应通道多路化(根修 M-B5 + RPC 架构)

**Files:**
- Modify: `crates/localtrans-core/src/session.rs`(take/return_inbound_resp_rx → 按 msg_id 路由的 `DashMap<u64, oneshot::Sender>` 或 `broadcast`)
- Modify: `crates/localtrans-core/src/transfer/engine.rs`(RPC 调用点带 msg_id)
- Modify: FFI/Tauri 调用点跟随
- Test: 既有 e2e 全量 + 并发 RPC 新测试

- [ ] **Step 1: 失败测试**:`并发 3 个 list_remote 不同目录同时进行,互不报"通道被占用"`
- [ ] **Step 2:** FAIL
- [ ] **Step 3:** 每请求生成 `msg_id: u64`(原子递增),`send_rpc(msg, id)` 注册 oneshot,响应侧路由分发;通道占用错误类型删除;超时统一 6s;FFI `list_remote`/`remote_shares`/`share_op` 与桌面壳同步迁移
- [ ] **Step 4:** e2e 全绿 + 双壳编译;`git commit -m "refactor(rpc): 响应通道按 msg_id 多路化——浏览与传输互斥消除"`

### Task 17: FFI OnceLock 快照重构(根修 M-B6)

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs`(state: `Mutex<Option<Arc>>` → `OnceLock<Arc<AppState>>` + init 期 Mutex)
- Test: ffi 既有测试

- [ ] **Step 1:** init 前 `new()` 内部仍 Mutex 保护构造;`start()` 完成时 `OnceLock::set(Arc)`;此后所有读路径 `get().expect("not started")` 转为返回 `AppException::Internal("尚未初始化")`;shutdown 走单独 `AtomicBool`
- [ ] **Step 2:** `cargo test -p localtrans-ffi -- --test-threads=1` + so 重编 + 绑定 + 补丁;gradle 编译
- [ ] **Step 3:** `git commit -m "refactor(ffi): 状态 OnceLock 快照化——读路径无锁,毒锁面清零"`

### Task 18: 秒传 oracle 完整方案 + CSP 命令面延伸

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs`(skip 语义:接收方回 bitmap 不回显 hash)
- Modify: `src-tauri/src/commands.rs`(respond_offer.save_dir 校验位于授权目录)
- Test: core e2e 秒传用例断言调整

- [ ] **Step 1: 失败测试**:`OfferResp(accepted) 中不含任何完整 hash 字符串——skip 改为与请求 files 等长的位图 Vec<bool>`
- [ ] **Step 2:** FAIL
- [ ] **Step 3:** skip 改 `Vec<bool>`(与 files 逐位对应),发送方按位剔除;save_dir 校验:`respond_offer` 的目录必须 == 设置的下载目录或其子目录(canonicalize + starts_with);全量回归 + 双壳编译;台账记阶段三完成
- [ ] **Step 4:** `git commit -m "refactor(security): 秒传位图化与 save_dir 目录校验"`

## 阶段四:Low 清扫 + 收尾(25 项,Task 19-24)

### Task 19: core/relay Low 批(cooldown/超时/分页/常时比较等)

**Files(审计第六节对应)**: `session.rs(cooldown 清理+ctrl send 5s 超时+MetaResp>4MiB 分页),share_watch.rs(spawn_blocking),engine.rs(small_batch_streams 门禁),dedup.rs(批量保存+唯一 tmp 名),relay config.rs(摘要 16 hex),control.rs(PSK ct_eq+hidden Punch 统一回 ok:false+带宽计数字段预留)`

- [ ] **Step 1:** 逐项小改(每项独立可 grep 验证:cooldown 处 remove、`timeout(5s)` 包装、`MAX_META_RESP` 分页、`#[cfg(test)]` 门、tmp 名带 pid、`subtle::ct_eq`、hidden 目标 Punch 统一 `ok:false, reason:"不可达"`)
- [ ] **Step 2:** `cargo test` 三 crate 回归;`git commit -m "fix(low): core/relay 低危批次(12 项)"`

### Task 20: 双壳 Low 批(通知方向/搜索派生/监听器竞态等)

**Files**: `TransferNotifier.kt(方向过滤),FilesViewModel.kt(全量派生),PairingDialog.vue(注销竞态),ffi lib.rs(saved_files 清理+set_inbox_dir 竞态+Done 后 remove),src-tauri main.rs(push migrate 对齐 FFI),FilesScreen/DocsTab(stat 移 IO),TransfersViewModel(死 ticker 删),usage 文档(私有目录说明)`

- [ ] **Step 1:** 逐项实现(Ticker 删除、`notify` 判 direction、entries 全量 + query combine、PairingDialog 同步收集数组 + isUnmounted 标志、saved_files Done 清理、migrate `direction=="push"` 只改状态)
- [ ] **Step 2:** gradle + cargo 回归;`git commit -m "fix(low): 双壳低危批次(13 项)"`

### Task 21: 版本对齐 + CHANGELOG

**Files:** `Cargo.toml, src-tauri/Cargo.toml, src-tauri/tauri.conf.json, android/app/build.gradle.kts(versionCode 15 + "0.11.0"), CHANGELOG.md`

- [ ] **Step 1:** 四处版本 0.10.3→0.11.0;CHANGELOG 按四阶段摘要(安全修复列信任链三连/服务防御/数据泄露专项;稳定性列永久瘫痪三件套;架构列 RPC 多路化/OnceLock)
- [ ] **Step 2:** 三端编译验证;`git commit -m "chore: v0.11.0 版本对齐与 CHANGELOG"`

### Task 22: 三端打包 + 隐私核查

- [ ] PC release 编译 + relay win/ubuntu(WSL /root/build,sed 排除 src-tauri)+ APK(so 重编→绑定重生成→errorMessage 补丁→assembleRelease 验签)
- [ ] dist 三件套组装 + zip;隐私核查(Add-Type ZipFile 检查 0 data/)
- [ ] `git commit -m "chore: v0.11.0 三端打包与隐私核查"`

### Task 23: 审计报告状态标注 + tag

- [ ] `docs/audit/2026-08-26-fullrepo-audit.md` 每条 finding 后加 `(✅ 已修 commit <hash>)` 映射
- [ ] tag v0.11.0 打在收尾提交;`git commit -m "docs: 审计报告 76 项修复状态闭环"`

### Task 24: 装机双机验证清单

- [ ] 升级路径:v0.10.3 数据目录被 v0.11.0 正常读取
- [ ] 中继互通:新 relay + 双端,发现/配对/互传全走钉扎握手
- [ ] 崩溃注入:空指纹 push 不崩;双开占端口 start() 报错不崩
- [ ] 传输中断恢复:中断 10 次后传输功能正常
- [ ] 泄露复查:logcat 无配对码/文件名;剪贴板 45s 自清;PSK 显示掩码
- [ ] 台账收尾:`.superpowers/sdd/progress.md` 记 Task 1-24 全收官

---

## Self-Review 记录

- 覆盖:spec §4(28 项)→ Task 1-9;§5(19 项)→ Task 10-15;§6(8 项)→ Task 16-18;§7(25 Low + 收尾)→ Task 19-24;§8 测试分布各 Task 内;§10 交付物 → Task 21-23
- 无占位符;锚点行号基于 v0.10.3 基线,实现者以 grep 重新定位为准(行号漂移容差已在任务内注明锚点关键词)
- 类型一致性:decode_fp32/BufferGuard/msg_id/generation 等新接口在 Produces 块声明,下游任务 Consumes 对应
