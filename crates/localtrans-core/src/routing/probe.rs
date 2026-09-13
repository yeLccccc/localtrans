//! M3b FR2 探测原语:阶梯带宽 + Ping/Pong RTT,全部走**独立探测 bi 流**。
//!
//! # wire 兼容定案(任务卡红线,想清楚的权衡)
//!
//! 备选方案 A(已否决):探测消息走 ctrl 流。对旧端=serde 未知变体 →
//! `take_msg` 解码失败 → ctrl_loop 断连——第一次探测就把健康会话打断,
//! 必须配能力协商/版本门,而既有 OfferResp 时代没有可用协商位,广播包
//! 6 字段红线又禁止加 feature 位。否决。
//!
//! **定案方案 B(本实现):探测在既有 QUIC 会话上新开 bi 流**(计划卡
//! "探测用独立 bi 流,复用会话"原文)。会话连接上唯一的 accept_bi 消费者
//! 是握手期的 ctrl 流(被取走后,响应端任务 [`serve_probe_streams`] 成为
//! 唯一 accept 循环,只认探测消息):
//! - **新→旧**:旧端没有探测流消费者(quinn 不自动拒收),数据只在其
//!   接收窗口内缓冲,旧端 ctrl_loop 永远看不到新变体、不解析、不断连、
//!   零感知;探测方等不到响应 → 超时 → 通道记录拉黑探测(`ChannelTable::
//!   record_probe_sample`,第一次失败即停;连接断了重连后标记丢失可接受,
//!   下次会话 register 补测)。**老端连接无损,无需协商/版本门**。
//! - **新→新**:响应端回 Pong/ProbeResp,全量探测成立。
//! - **ctrl 流收到的探测类消息**(异常/第三方实现误投):忽略不断连
//!   (session.rs ctrl_loop 防御分支)——回包或断连都会伤及混版本组网。
//!
//! # 带宽语义:出站速率版
//!
//! 发端发 ProbeReq{size} + size 字节裸数据,对端**读满即丢**,回
//! ProbeResp。计时=发出起至收到应答止——应答只有在 size 字节真正走完
//! 路径后才可能到达(对端读完才回),故测得的是端到端送达速率(同时被
//! 流控背压与路径容量约束),对"本端向对端发文件"的选路正是需要的方向。
//! 精确收速率(对端回灌数据)留升级路径:反转探测方向即可,wire 已备好
//! (两端都有响应端,任一方可主动发起)。
//!
//! 阶梯:64KB→512KB→4MB(设计 16 §2.1,4MB 穿透 QUIC 慢启动;
//! 总开销 ~5MB/对端/次,不打满带宽红线)。取后两级速率均值
//! ([`ladder_est_bps`],首级只做慢启动预热)。

use super::{ChannelTable, Fingerprint, ProbeKind};
use crate::protocol::{encode_control, ControlMsg, ProtocolError};
use quinn::{Connection, RecvStream, SendStream};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

/// 阶梯三级尺寸(设计 16 §2.1:64KB → 512KB → 4MB)
pub const TIER_SIZES: [usize; 3] = [64 * 1024, 512 * 1024, 4 * 1024 * 1024];

/// RTT Ping 次数(×3 取中位)
pub const PING_ATTEMPTS: usize = 3;

/// 单次 Ping 等待应答超时(对端不应答=旧版本/路径坏,尽快放弃)
pub const PING_TIMEOUT: Duration = Duration::from_secs(2);

/// 探测数据量上限(响应端防御:超过直接弃流;与最大阶梯级一致)
pub const MAX_PROBE_BYTES: u32 = TIER_SIZES[2] as u32;

/// ProbeReq 数据部分的读超时按"1Mbps 底线速率 + 15s 余量"估算:
/// 慢路径(中继/弱网)给足时间,死路径不至于久等。
pub fn tier_timeout(size: usize) -> Duration {
    let millis = 15_000 + size as u64 * 8_000 / 1_000_000; // size 字节 @1Mbps 的毫秒数
    Duration::from_millis(millis)
}

/// 阶梯估算:取后两级速率的(向下取整)均值——"后两级的中位段速率",
/// 首级只做慢启动预热。纯函数,单测钉死契约。
pub fn ladder_est_bps(tier_rates_bps: [u64; 3]) -> u64 {
    tier_rates_bps[1].saturating_add(tier_rates_bps[2]) / 2
}

/// 探测 nonce 计数器(进程内唯一即可;探测流有 TLS 与指纹钉扎保护)
fn next_nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

/// 探测失败原因(调用方只区分"记失败样本",不细分)
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("对端无响应(超时)")]
    Timeout,
    #[error("连接/流错误: {0}")]
    Io(String),
    #[error("协议错误: {0}")]
    Protocol(String),
}

impl From<std::io::Error> for ProbeError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => ProbeError::Timeout,
            _ => ProbeError::Io(e.to_string()),
        }
    }
}

impl From<quinn::ConnectionError> for ProbeError {
    fn from(e: quinn::ConnectionError) -> Self {
        ProbeError::Io(e.to_string())
    }
}

impl From<quinn::WriteError> for ProbeError {
    fn from(e: quinn::WriteError) -> Self {
        ProbeError::Io(e.to_string())
    }
}

impl From<quinn::ReadExactError> for ProbeError {
    fn from(e: quinn::ReadExactError) -> Self {
        match e {
            quinn::ReadExactError::FinishedEarly(_) => ProbeError::Io("流已结束".into()),
            quinn::ReadExactError::ReadError(read_err) => match read_err {
                quinn::ReadError::Reset(code) => ProbeError::Io(format!("流被重置: {code}")),
                other => ProbeError::Io(other.to_string()),
            },
        }
    }
}

impl From<ProtocolError> for ProbeError {
    fn from(e: ProtocolError) -> Self {
        ProbeError::Protocol(e.to_string())
    }
}

// ================= 发起端 =================

/// 在既有会话上做**全量探测**(会话建立/补测/升级复测):
/// Ping×3 RTT(中位) → 阶梯三级带宽。每次 Ping/阶梯结果写入通道记录;
/// Ping 全败视为探测不支持/路径坏,记失败样本并跳过阶梯(第一次失败即
/// 拉黑,不给旧版本对端反复白耗 5MB 阶梯流量的机会)。返回带宽估算供调用方升级判定。
pub async fn probe_full(
    conn: &Connection,
    table: &ChannelTable,
    fp: &Fingerprint,
    addr: &std::net::SocketAddr,
) -> Option<u64> {
    // RTT:Ping×3 取中位;每次尝试都是一个稳定性样本
    let mut rtts = [0u64; PING_ATTEMPTS];
    let mut oks = 0usize;
    for slot in rtts.iter_mut() {
        match ping_once(conn).await {
            Ok(ms) => {
                *slot = ms;
                oks += 1;
                table.record_probe_sample(fp, addr, true);
            }
            Err(e) => {
                tracing::debug!("探测 Ping 失败({addr}): {e}");
                table.record_probe_sample(fp, addr, false);
            }
        }
    }
    table.reap_timeouts(fp);
    if oks == 0 {
        tracing::info!("探测不支持或路径不可用({addr}),通道记录已拉黑探测");
        return None;
    }
    table.record_rtt(fp, addr, super::median3(rtts));

    // 带宽阶梯:三级顺序计时;任一级失败则记样本并以已得级别兜底
    let mut rates = [0u64; 3];
    let mut done = 0usize;
    for (i, &size) in TIER_SIZES.iter().enumerate() {
        match ladder_once(conn, size).await {
            Ok(bps) => {
                rates[i] = bps;
                done = i + 1;
            }
            Err(e) => {
                tracing::debug!("阶梯第 {} 级失败({addr}): {e}", i + 1);
                table.record_probe_sample(fp, addr, false);
                table.reap_timeouts(fp);
                break;
            }
        }
    }
    if done > 0 {
        table.record_probe_sample(fp, addr, true);
        // 未跑完全梯(中途失败)时,已得级别的最后两级均值仍可作保守估算
        let est = match done {
            1 => rates[0],
            2 => ladder_est_bps([0, rates[0], rates[1]]),
            _ => ladder_est_bps(rates),
        };
        table.record_bandwidth(fp, addr, ProbeKind::Full, est);
        Some(est)
    } else {
        None
    }
}

/// 在既有会话上做**快检**(5min 周期:1 次 Ping 保 RTT/稳定性新鲜 + 64KB 一级)。
/// 返回快检带宽 bps;None=失败(已记样本,调度器据此拉黑/摘除)。
pub async fn probe_quick(
    conn: &Connection,
    table: &ChannelTable,
    fp: &Fingerprint,
    addr: &std::net::SocketAddr,
) -> Option<u64> {
    match ping_once(conn).await {
        Ok(ms) => {
            table.record_probe_sample(fp, addr, true);
            table.record_rtt(fp, addr, ms);
        }
        Err(e) => {
            tracing::debug!("快检 Ping 失败({addr}): {e}");
            table.record_probe_sample(fp, addr, false);
            table.reap_timeouts(fp);
            return None;
        }
    }
    match ladder_once(conn, TIER_SIZES[0]).await {
        Ok(bps) => {
            table.record_probe_sample(fp, addr, true);
            table.record_bandwidth(fp, addr, ProbeKind::Quick, bps);
            Some(bps)
        }
        Err(e) => {
            tracing::debug!("快检 64KB 失败({addr}): {e}");
            table.record_probe_sample(fp, addr, false);
            table.reap_timeouts(fp);
            None
        }
    }
}

/// 单次 RTT 测量:开探测流发 Ping,等 Pong,返回往返毫秒。
pub async fn ping_once(conn: &Connection) -> Result<u64, ProbeError> {
    let nonce = next_nonce();
    let started = Instant::now();
    let (mut send, mut recv) = conn.open_bi().await?;
    write_framed(&mut send, &ControlMsg::Ping { nonce }).await?;
    // 发送方向即关(半关闭):对端不应答时流资源也能被两端尽早回收
    send.finish().map_err(|_| ProbeError::Io("finish 失败".into()))?;
    let pong = tokio::time::timeout(PING_TIMEOUT, read_framed(&mut recv))
        .await
        .map_err(|_| ProbeError::Timeout)??;
    match pong {
        ControlMsg::Pong { nonce: n } if n == nonce => Ok(started.elapsed().as_millis() as u64),
        other => Err(ProbeError::Protocol(format!("期望 Pong({nonce}),实得 {other:?}"))),
    }
}

/// 单级带宽测量:开探测流发 ProbeReq{size} + size 字节,等 ProbeResp。
/// 计时含全程(见模块注释"出站速率版")。返回估算 bps。
pub async fn ladder_once(conn: &Connection, size: usize) -> Result<u64, ProbeError> {
    let nonce = next_nonce();
    let started = Instant::now();
    let (mut send, mut recv) = conn.open_bi().await?;
    write_framed(&mut send, &ControlMsg::ProbeReq { size: size as u32, nonce }).await?;
    // 负载用确定性伪随机填充(防中间压缩/去重失真;xorshift 足够,不引 rand 依赖面)
    let mut chunk = vec![0u8; 64 * 1024];
    fill_pattern(&mut chunk, nonce ^ 0x9E37_79B9_7F4A_7C15);
    let mut left = size;
    let write = async {
        while left > 0 {
            let n = left.min(chunk.len());
            send.write_all(&chunk[..n]).await?;
            left -= n;
        }
        send.finish().map_err(|_| ProbeError::Io("finish 失败".into()))?;
        Ok::<_, ProbeError>(())
    };
    tokio::time::timeout(tier_timeout(size), write)
        .await
        .map_err(|_| ProbeError::Timeout)??;
    let resp = tokio::time::timeout(tier_timeout(size), read_framed(&mut recv)).await;
    let resp = match resp {
        Ok(r) => r?,
        Err(_) => return Err(ProbeError::Timeout),
    };
    let elapsed = started.elapsed();
    match resp {
        ControlMsg::ProbeResp { nonce: n } if n == nonce => {
            let ms = elapsed.as_millis() as u64;
            if ms == 0 {
                // 时钟粒度兜底:亚毫秒按 1ms 计(回环快路径)
                Ok((size as u64) * 1000)
            } else {
                Ok((size as u64) * 1000 / ms)
            }
        }
        other => Err(ProbeError::Protocol(format!("期望 ProbeResp({nonce}),实得 {other:?}"))),
    }
}

/// xorshift64 填充(确定性伪随机)
fn fill_pattern(buf: &mut [u8], seed: u64) {
    let mut x = seed | 1;
    for b in buf.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
}

// ================= 响应端 =================

/// 每会话探测响应端:accept 探测流并处理,直到连接关闭。
/// 由 session.rs 在会话建立时 spawn——新版本双端对称具备响应能力;
/// 旧版本无此任务(探测方超时拉黑,见模块注释 wire 权衡)。
pub async fn serve_probe_streams(conn: Connection) {
    while let Ok((send, recv)) = conn.accept_bi().await {
        tokio::spawn(async move {
            if let Err(e) = handle_probe_stream(send, recv).await {
                tracing::debug!("探测流处理结束: {e}");
            }
        });
    }
    tracing::debug!("探测响应端退出(连接已关闭)");
}

async fn handle_probe_stream(mut send: SendStream, mut recv: RecvStream) -> Result<(), ProbeError> {
    match read_framed(&mut recv).await? {
        ControlMsg::Ping { nonce } => {
            write_framed(&mut send, &ControlMsg::Pong { nonce }).await?;
            send.finish().map_err(|_| ProbeError::Io("finish 失败".into()))?;
            Ok(())
        }
        ControlMsg::ProbeReq { size, nonce } => {
            if size > MAX_PROBE_BYTES {
                // 超限弃流:不发应答,发端按超时处理
                return Err(ProbeError::Protocol(format!("探测量超限: {size}")));
            }
            // 读满即丢(背压靠读;64KB 缓冲复用)
            let mut buf = vec![0u8; 64 * 1024];
            let mut left = size as usize;
            while left > 0 {
                let n = left.min(buf.len());
                recv.read_exact(&mut buf[..n]).await?;
                left -= n;
            }
            write_framed(&mut send, &ControlMsg::ProbeResp { nonce }).await?;
            send.finish().map_err(|_| ProbeError::Io("finish 失败".into()))?;
            Ok(())
        }
        other => Err(ProbeError::Protocol(format!("探测流首消息非法: {other:?}"))),
    }
}

// ================= 帧收发(与 ctrl 流同构:4B BE 长度前缀 + JSON) =================

async fn write_framed(send: &mut SendStream, msg: &ControlMsg) -> Result<(), ProbeError> {
    let data = encode_control(msg);
    send.write_all(&data).await?;
    send.flush().await?;
    Ok(())
}

async fn read_framed(recv: &mut RecvStream) -> Result<ControlMsg, ProbeError> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > crate::protocol::CHUNK_SIZE {
        return Err(ProbeError::Protocol(format!("帧长非法: {len}")));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await?;
    Ok(crate::protocol::decode_control_body(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::ChannelTable;

    #[test]
    fn 阶梯估算_取后两级均值() {
        assert_eq!(ladder_est_bps([100, 800, 900]), 850, "后两级均值,首级预热丢弃");
        assert_eq!(ladder_est_bps([0, 0, 100]), 50);
        assert_eq!(ladder_est_bps([1, 3, 4]), 3, "向下取整均值");
        assert_eq!(ladder_est_bps([0, 0, 0]), 0);
        // 溢出防御:saturating 加法后取半,不 panic
        assert_eq!(ladder_est_bps([0, u64::MAX, u64::MAX]), u64::MAX / 2);
    }

    #[test]
    fn 级超时_按1m底线加余量() {
        // 64KB @1Mbps = 0.524s → 524ms
        assert_eq!(tier_timeout(64 * 1024), Duration::from_millis(15_000 + 524));
        // 4MiB @1Mbps = 33.554s
        assert_eq!(tier_timeout(4 * 1024 * 1024), Duration::from_millis(15_000 + 33_554));
        assert_eq!(tier_timeout(0), Duration::from_millis(15_000));
    }

    #[test]
    fn 负载填充_确定性且非全零() {
        let mut a = vec![0u8; 1024];
        let mut b = vec![0u8; 1024];
        fill_pattern(&mut a, 42);
        fill_pattern(&mut b, 42);
        assert_eq!(a, b, "同种子确定性");
        fill_pattern(&mut b, 44); // 注意种子经 |1 取奇:42/43 同序列,须隔开
        assert_ne!(a[..64], b[..64], "不同种子不同序列");
        assert!(a.iter().any(|&x| x != 0), "非全零(防压缩失真)");
    }

    // ===== 环回集成:127.0.0.1 双 listener 模拟双地址(任务卡验证要求) =====

    /// 全量探测走真实 QUIC 会话(响应端=insert_session_and_spawn 挂的任务):
    /// 双地址(两个端口)各自记录 RTT/带宽/稳定性样本,评分齐备后 decide 可用;
    /// 附带验证 ctrl 流隔离——把 Ping 误投 ctrl 流,会话必须无损(防御分支忽略)。
    #[tokio::test]
    async fn 环回_双地址_全量探测_记录与ctrl隔离() {
        use crate::identity::{Perms, TrustedPeer};
        use crate::protocol::ControlMsg;
        use crate::routing::score::decide;
        use crate::test_support::{init_tracing, setup_ctx, start_listener};
        use std::time::Duration;

        init_tracing();
        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");
        {
            let mut ta = ctx_a.trust.lock().await;
            ta.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".into(),
                alias: String::new(),
                paired_at: 1,
                perms: Perms::default(),
            });
            let mut tb = ctx_b.trust.lock().await;
            tb.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".into(),
                alias: String::new(),
                paired_at: 1,
                perms: Perms::default(),
            });
        }

        // 地址1:托管 listener;地址2:第二 listener(裸端点 + adopt_connection,
        // 让乙侧会话管理器为其挂上探测响应端)
        let addr1 = start_listener(&sm_b).await;
        let ep2 = crate::session::bind_endpoint(0, &ctx_b.identity).unwrap();
        let mut addr2 = ep2.local_addr().unwrap();
        addr2.set_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        assert_ne!(addr1, addr2, "双地址=本机两个端口");
        tokio::spawn(async move {
            while let Some(incoming) = ep2.accept().await {
                if let Ok(conn) = incoming.await {
                    let _ = sm_b.adopt_connection(conn).await;
                }
            }
        });

        // 甲经地址1建托管会话(响应端随会话挂上)
        let got_fp = tokio::time::timeout(Duration::from_secs(5), sm_a.connect(addr1))
            .await
            .expect("连接超时")
            .expect("连接失败");
        assert_eq!(got_fp, fp_b);
        // 先取地址1 的连接句柄,再经地址2 建第二条托管会话(会话表代次覆盖,
        // 但旧连接句柄仍存活可用——双通道探测正需要两条活连接)
        let conn1 = sm_a.session(&fp_b).await.expect("托管会话应存在");
        tokio::time::timeout(Duration::from_secs(5), sm_a.connect_pinned(addr2, fp_b))
            .await
            .expect("连接2 超时")
            .expect("连接2 失败");
        let conn2 = sm_a.session(&fp_b).await.expect("第二代会话应存在");

        // 双地址各跑一次全量探测(乙侧响应端消费 Ping/ProbeReq)
        let table = ChannelTable::new();
        table.note_connected(&fp_b, addr1, false);
        table.register(&fp_b, addr2, false);
        let est1 = tokio::time::timeout(Duration::from_secs(60), probe_full(&conn1, &table, &fp_b, &addr1))
            .await
            .expect("全量探测超时")
            .expect("通道1 探测应成功");
        let est2 = tokio::time::timeout(Duration::from_secs(60), probe_full(&conn2, &table, &fp_b, &addr2))
            .await
            .expect("全量探测超时")
            .expect("通道2 探测应成功");

        // 记录断言:双地址齐备、RTT/带宽在合理界内、稳定性全成功
        let snap = table.snapshot(&fp_b);
        assert_eq!(snap.len(), 2, "双地址各一条记录");
        for r in &snap {
            let rtt = r.rtt_ms.expect("RTT 应有值");
            let bps = r.est_bps.expect("带宽应有值");
            assert!(rtt < 1000, "环回 RTT 应远小于 1s,实际 {rtt}ms");
            assert!(bps > 1_000_000, "环回带宽应远大于 1Mbps,实际 {bps}bps");
            assert!(r.loss_window.iter().all(|&ok| ok), "环回不应有探测丢失");
            assert_eq!(r.last_full_bps, r.est_bps, "全量探测刷新基准");
        }
        let snap1 = table.snapshot(&fp_b).into_iter().find(|r| r.addr == addr1).unwrap();
        let snap2 = table.snapshot(&fp_b).into_iter().find(|r| r.addr == addr2).unwrap();
        // 全量基准分离锚:est1/est2 均为全量结果
        assert_eq!(snap1.est_bps, Some(est1));
        assert_eq!(snap2.est_bps, Some(est2));

        // 评分齐备 → 决策可用:无在位通道返回两地址之一(环回双通道分差小,
        // 直连同权,不硬断言具体哪条——迟滞/择优已有纯函数直测)
        let choice = decide(&table.snapshot(&fp_b), None);
        assert!(choice == Some(addr1) || choice == Some(addr2), "决策应落在双地址内");

        // ctrl 流隔离:Ping 误投 ctrl 流 → 乙侧防御分支忽略,会话无损
        sm_a.send_ctrl(&fp_b, ControlMsg::Ping { nonce: 99 }).await.expect("发送失败");
        // 监视 800ms:会话若断连即时失败;全程存活则超时退出(即通过)
        let watched = tokio::time::timeout(Duration::from_millis(800), async {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                assert!(
                    sm_a.session(&fp_b).await.is_some(),
                    "ctrl 流误投 Ping 导致断连——防御分支失效"
                );
            }
        })
        .await;
        assert!(watched.is_err(), "监视循环提前退出");
        assert!(sm_a.session(&fp_b).await.is_some(), "会话应仍存活");

        // 收尾:关托管会话,各任务自然退出
        drop(conn2);
        sm_a.shutdown_all().await;
    }
}
