# 跨端取消通知与 source 任务清理 设计文档

> 版本目标:v0.6.x | 前置:2026-08-23 提交 40081a8(RecvAck 补齐)
> 状态:待用户审阅

## 1. 问题陈述

任务行进终态的契约目前只覆盖"**正常完成**":

- **pull 正常完成**:接收方 finalize 后回 `RecvAck{job_id}` → 发送方路由器清理 sender 任务 + 发 `SourceDone` → 壳层落 done、盖 `finished_at_ms` 戳(v0.2.4 引入,40081a8 补齐空文件/推送大文件两处漏发)
- **push 正常完成**:接收方整 offer 落盘后回 `JobDone{offer_id}` → 甲侧 `push_files` 返回 Ok → 壳层命令泵落 done

**失败/取消路径没有跨端通知**。发送方(数据源)的 source 行在以下场景永久卡在"进行中"(active),计时不停、进度泵不降频:

| # | 场景 | 甲方(发起方) | 乙方(数据源)source 行 |
|---|---|---|---|
| S1 | 甲下载中途点"取消" | 本地 failed ✓ | **无任何消息**——连接仍活着时 30s 连接关闭兜底永不触发;探针 60s 空闲自杀只停探针,不清任务表、不发事件 → **永久 active** |
| S2 | 甲推送,乙接收失败(如磁盘满) | push 行 failed ✓(JobFailed 驱动) | 乙为大文件反向取流产生的**甲方 source 行**无人清理 → 永久 active |
| S3 | 甲推送中途取消 | push 行 failed ✓ | 甲方自己的 source 行:小文件由批流循环检 cancelled 退出;大文件 source 行同 S2 无人清理 → 永久 active |
| S4 | 网络断/对端掉线 | 双方各自收敛 | 30s 连接关闭兜底清理 ✓(慢但正确,**不在本 spec 范围**) |

S1~S3 是本 spec 要修的缺口。S4 已有兜底,不动。

### 明确不做的事(设计决策)

- **探针 60s 空闲自杀不改成"清理任务表"**。长暂停场景(对端 Pause 后字节停增)探针会自杀,但注册表条目必须活着——否则对端恢复并完成后回的 `RecvAck` 会找不到任务,`SourceDone` 发不出来,source 行回到"永久 active"。探针自杀只停探针,是正确行为。此前的错误直觉(把静默退出改成有终态退出)会破坏暂停/恢复,否决。

## 2. 方案总览

两个独立机制,分别覆盖 S1 与 S2/S3:

```
机制 A:JobCancel 消息(S1)
  甲(下载取消)                        乙(数据源)
  transfer_action("cancel")
    ├─ 本地:任务落 failed(现有)
    └─ 新增:send_ctrl(JobCancel{job_id}) ──→ 路由器收到:
                                              remove sender_jobs[job_id]
                                              fire_probe_stop
                                              发 SourceFailed{job_id,"对方已取消"}

机制 B:offer_id 关联清理(S2/S3)
  push_files 失败返回(任何错误路径)      甲自己的 sender 任务
    └─ 新增:按 offer_id 找关联 sender 任务   ←─ SenderJobState 新增 offer_id 字段
       (push: 前缀解析时存入)
       remove + fire_probe_stop
       发 SourceFailed{job_id, reason}
```

协议不新增"失败通知"消息——S2 的乙方侧(接收方自己的行)已由 `JobFailed` 驱动 failed,无需额外处理;要补的只是**甲方的 source 行**。S3 同理由机制 B 覆盖。

## 3. 详细设计

### 3.1 协议消息(protocol.rs)

```rust
ControlMsg::JobCancel {
    job_id: u64,
},
```

- serde:与 RecvAck 同款(普通 u64,bincode 编码,无兼容问题)
- **消息分发归类(session.rs 控制循环)**:与 `RecvAck` 同类——转发到**入站控制通道**(inbound_ctrl)给 RPC 路由器处理,而非入站响应通道。理由:它的消费者是发送方路由器(清理 sender 任务),不是正在等待响应的传输任务
- 两端同版本升级,项目一贯无混跑,无需版本协商

### 3.2 乙方路由器处理(engine.rs spawn_rpc_router)

在 `ControlMsg::RecvAck` 分支旁新增:

```rust
ControlMsg::JobCancel { job_id } => {
    // 甲侧取消拉取:清理 sender 任务并落终态(source 行不再永久 active)
    let state = sender_jobs.write().await.remove(&job_id);
    if let Some(state) = state {
        fire_probe_stop(&state);
        let _ = state.progress_tx.send(ProgressEvent::SourceFailed {
            job_id,
            reason: "对方已取消".to_string(),
        }).await;
        tracing::info!("收到 JobCancel: job {} 清理(对方取消)", job_id);
    } else {
        tracing::debug!("JobCancel 目标任务不存在(可能已完成/已清理): job {}", job_id);
    }
}
```

- 幂等:任务不存在时仅 debug 日志(与 RecvAck 处理同款语义——取消迟到于完成时静默忽略)
- job_id 是 source 段 id(0x8000... 起),与 sender_jobs 键一致,无需转换

### 3.3 甲方取消时发送(壳层 commands.rs transfer_action)

`transfer_action` 的拉取回退分支(control_task 成功后)新增:

```rust
// 取消是跨端事件:通知数据源清理 sender 任务(source 行落终态)。
// 任务可能已结束(control_task 返回 false 的场景不走这里),或对端已
// 先一步完成(乙侧 JobCancel 处理幂等)——发送失败也只影响对端清理
// 时效,不影响本机语义。
if action == "cancel" {
    let _ = state.sm.send_ctrl_fp(job_id_peer, ControlMsg::JobCancel { job_id }).await;
}
```

要点:
- **需要 peer 指纹**:从任务表 DTO 的 `peer` 字段(hex)解码。任务表有 peer 值(Started 事件写入)→ 直接可用
- 发送是**尽力而为**(`let _ =`):连接已断时 send_ctrl 失败,此时乙方本来就有 30s 连接关闭兜底,双保险
- sender_jobs / push_control 分支(本机是 source 的取消)不发 JobCancel——那两类的"对端行"由机制 B 或接收方的 JobFailed 处理,不重复通知

### 3.4 SenderJobState 关联 offer_id(sender_state.rs + engine.rs)

```rust
pub struct SenderJobState {
    // ...现有字段...
    /// 关联的推送 offer_id(大文件推送的反向取流任务)。None = 普通 pull。
    /// push_files 失败路径按它清理关联 sender 任务(机制 B)。
    pub offer_id: Option<u64>,
}
```

赋值点:spawn_rpc_router 的 MetaReq 分支,解析 `push:{offer_id:x}` 前缀成功时(现有 FIX B 桥接处,~engine.rs:1494)`state.offer_id = Some(offer_id)`,普通 pull 保持 None。

### 3.5 push_files 失败路径清理(engine.rs push_files_rel)

`push_files_rel` 在返回 Err 前(所有错误路径收敛点)新增清理:

```rust
// 失败路径:清理本 offer 关联的 sender 任务(大文件反向取流产生的
// source 行),否则它们永久 active(计时不停)。通过进程级 sender_jobs
// 注册表按 offer_id 反查。
async fn cleanup_push_senders(
    sm: &SessionManager,
    sender_jobs: &SenderJobMap,
    offer_id: u64,
    reason: &str,
) {
    let mut jobs = sender_jobs.write().await;
    let ids: Vec<u64> = jobs.iter()
        .filter(|(_, s)| s.offer_id == Some(offer_id))
        .map(|(id, _)| *id)
        .collect();
    for id in ids {
        if let Some(state) = jobs.remove(&id) {
            fire_probe_stop(&state);
            let _ = state.progress_tx.send(ProgressEvent::SourceFailed {
                job_id: id,
                reason: reason.to_string(),
            }).await;
        }
    }
}
```

调用点与理由:
- **等待 OfferResp 超时/被拒后**:大文件还没开始,无 sender 任务,调用无害(空集)
- **等待 JobDone 超时/JobFailed 返回 Err 前**(engine.rs:944 `?` 处改为先清理再返回):这是 S2/S3 的主场景
- **Cancelled 路径**(批流循环/控制标志触发):S3
- 注意:清理走**进程级注册表**(spawn_rpc_router 建的那份,经 SessionManager 或全局入口获取——实现时确认 sender_jobs 的传递方式,可能需要把 map 传进 push_files_rel 或提供进程级 static 入口,与 push_jobs() 同款模式)

实现取舍:优先采用 `push_jobs()` 同款的**进程级 static 注册表模式**(sender_jobs 在 MetaReq 分支已可访问;为 push 侧清理提供独立入口),避免给 push_files_rel 加参数连锁改签名。

### 3.6 壳层与前端

**零改动**。`SourceFailed` 事件桥已就位(桌面 main.rs / 安卓 ffi 均处理),source 行落 failed + 盖 `finished_at_ms` 戳,计时冻结。

## 4. 测试计划(TDD)

### T1:JobCancel 乙方清理(core,红→绿)
`cancel_pull_notifies_source_side`:两实例互信,甲 start_pull 中途(收到 SourceStarted 后)调用 `transfer::control_task(job_id, Cancel)` 模拟取消**并直接 send_ctrl JobCancel**(或调用封装后的发送函数);断言乙方 source 通道收到 `SourceFailed{reason 含"取消"}` 且 sender_jobs 已清空。

### T2:push 失败清理关联 source 行(core,红→绿)
`push_failure_cleans_associated_source_jobs`:乙对甲推送 Auto 档;甲侧使大文件接收失败(如下载目录设为不可写路径);断言:push_files 返回 Err,**甲自己的 source 通道**收到 SourceFailed 且关联 sender 任务从注册表移除。

### T3:JobCancel 幂等(core,绿)
任务不存在时发 JobCancel 不 panic、仅 debug 日志(可并入 T1 断言:二次发送无副作用)。

### T4:暂停后恢复完成仍正常(core,回归)
现有探针自杀行为不破坏:`pause → 探针自杀 → resume → 完成` 全链路 RecvAck 仍能清理(source 行正常终态)。守护 3.1 节"明确不做"的边界。

### T5:协议序列化(core,绿)
`ControlMsg::JobCancel { job_id: 42 }` roundtrip(与 protocol.rs 现有 360 行同款风格)。

回归:core 118 全绿 + 壳层 19 绿 + workspace 编译 0 错。

## 5. 风险与边界

| 风险 | 评估 |
|---|---|
| JobCancel 迟到(任务已完成) | 乙方幂等处理(3.2),与 RecvAck 同语义 |
| 取消消息丢失(连接已断) | 乙方 30s 连接关闭兜底仍在(S4 不动) |
| offer_id 关联错漏 | 前缀解析已有(FIX B),只是存下来;同名/多文件靠 offer_id 唯一 |
| 暂停场景误清理 | 机制 B 只在 push_files **失败返回**时触发;暂停不返回,不触发 |
| 安卓端 | ffi 的 source 事件处理已就绪(lib.rs:1551),协议升级需两端重装——与 40081a8 同一约束,合并同次发版 |

## 6. 交付物

- protocol.rs:JobCancel 变体 + 分发归类 + 序列化测试
- engine.rs:乙方路由器分支、push 失败清理函数、MetaReq 存 offer_id
- sender_state.rs:offer_id 字段
- commands.rs(壳层):transfer_action 取消时发 JobCancel
- core 测试 ×4~5(红→绿)
- 无 UI 改动、无 ffi 改动

## 附录:实现修订(2026-08-23,计划阶段确定)

1. **不新增 JobCancel 消息**:复用既有 `TransferCtl{job_id, action}`——session.rs 已将其归入
   inbound_ctrl 转发类,engine.rs 路由器已有其分支(Throttle 已按 sender 侧处理),Cancel
   对称扩展。协议面零增量,原 3.1/3.2 的"新消息"由 TransferCtl 的 Cancel 分支承担。
2. **sender_jobs 参数穿透**(原 3.5 的实现取舍):注册表非进程级 static——壳层 AppState/
   ffi/测试各建 map 传给 spawn_rpc_router;`push_files`/`push_files_rel` 增加
   `sender_jobs: &SenderJobMap` 参数,清理与路由器操作同一 map,保持多实例测试隔离。
3. 原测试计划 T5(新消息序列化)随消息取消而取消;T4(暂停回归)以 Task 1 实现处的
   守护注释 + 既有 118 测试回归覆盖。
