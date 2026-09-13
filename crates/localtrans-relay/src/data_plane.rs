//! 数据面:每会话端口 UDP 互转发 + KNOCK 学习 + 令牌验证
//! 会话端口模型:Punch 命中时分配会话端口 S,双方都向 S 发包,中继在 S 上互转。

use crate::config::RelayConfig;
use crate::lease::{data_packet_ok, LeaseTable};
use localtrans_core::relay::proto::{
    data_header_decode, FLAG_DATA, FLAG_GOODBYE_KNOCK, FLAG_KNOCK,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::net::UdpSocket;

/// 会话端口学习表:port → (fp → 地址)
/// 每个会话端口最多两方(src_fp + dst_fp),DATA 时查对方转发
type LearnedTable = HashMap<u16, HashMap<[u8; 32], SocketAddr>>;

pub struct DataPlane {
    /// 全部端口池的 socket(设备租约端口 + 会话端口)
    sockets: Vec<Arc<UdpSocket>>,
    /// 端口 → socket 映射
    port_sock: HashMap<u16, Arc<UdpSocket>>,
    /// 租约表(用于令牌验证)
    leases: Arc<LeaseTable>,
    /// 学习表:会话端口 → (fp → 最近地址)
    learned: Arc<tokio::sync::RwLock<LearnedTable>>,
    /// 关闭标志
    shutdown: Arc<AtomicBool>,
    /// 低危审计修复(带宽计数预留):每端口收包字节累计(port → AtomicU64,不做限速)
    port_bytes_in: HashMap<u16, Arc<std::sync::atomic::AtomicU64>>,
}

impl DataPlane {
    /// 启动数据面:绑定端口池全部端口,每个端口一个 recv_loop
    pub async fn spawn(config: &RelayConfig, leases: Arc<LeaseTable>) -> Result<Arc<Self>, String> {
        let mut sockets = Vec::new();
        let mut port_sock = HashMap::new();

        // 绑定端口池全部端口(设备租约端口 + 会话端口共用)
        for port in config.data_port_start..config.data_port_end {
            let bind_addr = SocketAddr::new(
                if config.public_ip.is_ipv4() {
                    "0.0.0.0".parse().unwrap()
                } else {
                    "::".parse().unwrap()
                },
                port,
            );
            // socket2 预设 SO_REUSEADDRESS 再 bind:服务器重启/测试重绑场景
            // Windows 10048 会持续数秒(std bind 后设置无效,必须 bind 前)
            let s2 = socket2::Socket::new(
                if bind_addr.is_ipv4() { socket2::Domain::IPV4 } else { socket2::Domain::IPV6 },
                socket2::Type::DGRAM,
                Some(socket2::Protocol::UDP),
            ).map_err(|e| format!("数据面端口 {} socket 创建失败: {}", port, e))?;
            s2.set_reuse_address(true).map_err(|e| e.to_string())?;
            s2.bind(&bind_addr.into())
                .map_err(|e| format!("数据面端口 {} 绑定失败: {}", port, e))?;
            s2.set_nonblocking(true).map_err(|e| e.to_string())?;
            let std_sock: std::net::UdpSocket = s2.into();
            let sock = Arc::new(
                UdpSocket::from_std(std_sock)
                    .map_err(|e| format!("数据面端口 {} tokio 注册失败: {}", port, e))?,
            );
            sockets.push(sock.clone());
            port_sock.insert(port, sock);
        }

        // 低危审计修复(带宽计数预留):与 port_sock 键对应
        let port_bytes_in: HashMap<u16, Arc<std::sync::atomic::AtomicU64>> = port_sock.keys().copied()
            .map(|p| (p, Arc::new(std::sync::atomic::AtomicU64::new(0))))
            .collect();
        let dp = Arc::new(DataPlane {
            sockets,
            port_sock,
            leases,
            learned: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
            port_bytes_in,
        });

        // 每个端口一个 recv_loop
        for (port, sock) in dp.port_sock.iter() {
            let dp_clone = dp.clone();
            let sock = sock.clone();
            let counter = dp.port_bytes_in.get(port).cloned()
                .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicU64::new(0)));
            tokio::spawn(async move { dp_clone.recv_loop(sock, counter).await });
        }

        Ok(dp)
    }

    /// 低危审计修复(带宽计数预留):统计全部端口累计收包字节(debug 级日志可消费,不做限速)
    pub fn total_bytes_in(&self) -> u64 {
        self.port_bytes_in.values()
            .map(|c| c.load(Ordering::Relaxed))
            .sum()
    }

    /// 某端口累计收包字节(未绑定的端口返回 0)
    pub fn port_bytes_in(&self, port: u16) -> u64 {
        self.port_bytes_in.get(&port)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// 获取所有绑定的本地地址(测试用)
    pub fn local_addrs(&self) -> Vec<SocketAddr> {
        self.sockets.iter().map(|s| s.local_addr().unwrap()).collect()
    }

    /// 学习表某端口条目数(测试用,S3 断言)
    pub async fn learned_len(&self, port: u16) -> usize {
        self.learned.read().await.get(&port).map(|m| m.len()).unwrap_or(0)
    }

    /// 非成员拒绝计数(测试用,S3 断言)
    pub fn nonmember_rejected(&self) -> u64 {
        self.leases.nonmember_reject_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 关闭数据面
    pub async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // 给 recv_loop 时间退出
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    /// 收包循环:每个端口独立处理
    async fn recv_loop(
        self: Arc<Self>,
        sock: Arc<UdpSocket>,
        bytes_in: Arc<std::sync::atomic::AtomicU64>,
    ) {
        let mut buf = vec![0u8; 2048];

        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                break;
            }

            let (n, from) = match sock.recv_from(&mut buf).await {
                Ok(x) => x,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                Err(e) => {
                    tracing::warn!("数据面收包错误: {}", e);
                    continue;
                }
            };

            // 低危审计修复(带宽计数预留):累计收包字节,不做限速
            bytes_in.fetch_add(n as u64, Ordering::Relaxed);

            let pkt = &buf[..n];

            // 解 18B 头
            let (token, flag) = match data_header_decode(pkt) {
                Ok(x) => x,
                Err(_) => continue, // 坏头静默丢弃
            };

            // 载荷超限丢弃(所有包统一 1500B 限制)
            if !data_packet_ok(&pkt[18..]) {
                continue;
            }

            // 验令牌 → 指纹
            let Some(src_fp) = self.leases.get_by_token(&token) else {
                continue; // 未知令牌丢弃
            };

            // 收包端口 = 会话端口
            let port = match sock.local_addr() {
                Ok(addr) => addr.port(),
                Err(_) => continue,
            };

            // S3 会话成员校验:KNOCK/DATA/GOODBYE_KNOCK 只有会话双方能发
            // (非成员伪造 GOODBYE 可把在场者的学习表清掉、提前回收会话端口)
            if flag == FLAG_KNOCK || flag == FLAG_DATA || flag == FLAG_GOODBYE_KNOCK {
                if !self.leases.session_member_ok(port, &src_fp) {
                    tracing::warn!(
                        "会话端口 {} 拒绝非成员包: fp={}{}",
                        port, hex::encode(&src_fp[..8]),
                        if flag == FLAG_KNOCK { " (KNOCK)" } else { " (DATA)" }
                    );
                    continue;
                }
                // S6:成员包到达即刷新会话活跃时刻
                self.leases.touch_session(port);
            }

            match flag {
                FLAG_KNOCK => {
                    // KNOCK:只学习不转发
                    tracing::debug!("数据面收到 KNOCK: port={}, fp={}, from={}", port, hex::encode(src_fp), from);
                    let mut learned = self.learned.write().await;
                    learned.entry(port).or_default().insert(src_fp, from);
                    tracing::debug!("数据面学习表[{}] 现有 {} 个条目", port, learned.get(&port).map(|m| m.len()).unwrap_or(0));
                }

                FLAG_GOODBYE_KNOCK => {
                    tracing::debug!("GOODBYE_KNOCK 回收: fp={}", hex::encode(src_fp));
                    // 清除该 fp 在此端口的学习记录
                    let mut learned = self.learned.write().await;
                    if let Some(entry) = learned.get_mut(&port) {
                        entry.remove(&src_fp);
                        // 若该端口学习表已空(会话双方都走了),回收会话端口
                        if entry.is_empty() {
                            learned.remove(&port);
                            self.leases.remove_session_port(port);
                            tracing::debug!("会话端口 {} 双方均离开,已回收", port);
                        }
                    }
                }

                FLAG_DATA => {
                    // NAT 漂移检测:DATA 包源地址与学习表不一致 → 更新。
                    // 客户端 NAT 重绑定后发出的第一个数据包即触发重学习,
                    // QUIC 重传兜底丢包间隙(无需客户端重发 KNOCK)。
                    {
                        let learned = self.learned.read().await;
                        let drifted = learned
                            .get(&port)
                            .and_then(|addrs| addrs.get(&src_fp))
                            .is_some_and(|old| *old != from);
                        if drifted {
                            drop(learned);
                            let mut learned = self.learned.write().await;
                            if let Some(addrs) = learned.get_mut(&port) {
                                if let Some(old) = addrs.get(&src_fp).copied() {
                                    addrs.insert(src_fp, from);
                                    tracing::debug!(
                                        "NAT 漂移: port={}, fp={}, {} -> {}",
                                        port, hex::encode(src_fp), old, from
                                    );
                                }
                            }
                        }
                    }

                    // 转发给会话另一方(原逻辑不变)
                    let learned = self.learned.read().await;
                    let Some(addrs) = learned.get(&port) else {
                        continue;
                    };

                    // S3:精确寻址——目标 = 会话成员与 src 的差集(不再"第一个非 src")
                    let dst_fp = match self.leases.session_members_of(port) {
                        Some((a, b)) => {
                            if a == src_fp { Some(b) } else if b == src_fp { Some(a) } else { None }
                        }
                        // 无成员登记的端口(理论不可达,KNOCK 校验已挡)退回旧逻辑
                        None => addrs.keys().find(|&&fp| fp != src_fp).copied(),
                    };
                    let Some(dst_fp) = dst_fp else {
                        tracing::warn!("数据面: 端口 {} 找不到对方(src_fp={})", port, hex::encode(src_fp));
                        continue; // 对方未 KNOCK,丢弃
                    };

                    let Some(&dst_addr) = addrs.get(&dst_fp) else {
                        tracing::warn!("数据面: 端口 {} 找不到对方地址(dst_fp={})", port, hex::encode(dst_fp));
                        continue;
                    };

                    // 转发包(剥掉 18B 头,载荷直接从本 socket 发出)
                    let payload = &pkt[18..];
                    if let Err(e) = sock.send_to(payload, dst_addr).await {
                        tracing::warn!("转发失败 {}: {}", dst_addr, e);
                    }
                }

                _ => {
                    // 未知标志丢弃
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RelayConfig;
    use serial_test::serial;
    use localtrans_core::relay::proto::{data_header_encode, FLAG_DATA, FLAG_GOODBYE_KNOCK, FLAG_KNOCK};

    /// 测试:会话端口双向转发
    #[tokio::test]
    #[serial]
    async fn session_port_forwards_both_directions() {
        let config = RelayConfig::default_for_test();
        // 租约表使用与配置相同的端口池
        let leases = Arc::new(LeaseTable::new(config.data_port_start..config.data_port_end));
        let dp = DataPlane::spawn(&config, leases.clone()).await.unwrap();

        // 分配两个设备令牌
        let la = leases.alloc([1u8; 32], "A".into(), false).unwrap();
        let lb = leases.alloc([2u8; 32], "B".into(), false).unwrap();

        // 分配会话端口
        let session_port = leases.alloc_session_port([1u8;32],[2u8;32]).unwrap();
        let relay_ip = config.public_ip;
        let session_addr = SocketAddr::new(relay_ip, session_port);

        // A/B 的模拟 socket
        let sock_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

        // A 发 KNOCK
        let mut knock_a = Vec::new();
        data_header_encode(&mut knock_a, &la.token, FLAG_KNOCK);
        sock_a.send_to(&knock_a, session_addr).await.unwrap();

        // B 发 KNOCK
        let mut knock_b = Vec::new();
        data_header_encode(&mut knock_b, &lb.token, FLAG_KNOCK);
        sock_b.send_to(&knock_b, session_addr).await.unwrap();

        // 稍等学习生效
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // A 发 DATA(载荷 "hello")
        let mut data_a = Vec::new();
        data_header_encode(&mut data_a, &la.token, FLAG_DATA);
        data_a.extend_from_slice(b"hello");
        sock_a.send_to(&data_a, session_addr).await.unwrap();

        // B 应收到载荷 "hello"(源 = 中继:session_port)
        let mut buf = [0u8; 2048];
        let (n, from) = sock_b.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");
        assert_eq!(from, session_addr);

        // B 回 DATA(载荷 "ok")
        let mut data_b = Vec::new();
        data_header_encode(&mut data_b, &lb.token, FLAG_DATA);
        data_b.extend_from_slice(b"ok");
        sock_b.send_to(&data_b, session_addr).await.unwrap();

        // A 应收到载荷 "ok"(源 = 中继:session_port)
        let (n, from) = sock_a.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ok");
        assert_eq!(from, session_addr);

        dp.shutdown().await;
    }

    /// 测试:错令牌 DATA 包丢弃
    #[tokio::test]
    #[serial]
    async fn data_plane_drops_bad_token() {
        let config = RelayConfig::default_for_test();
        let leases = Arc::new(LeaseTable::new(config.data_port_start..config.data_port_end));
        let dp = DataPlane::spawn(&config, leases.clone()).await.unwrap();

        let la = leases.alloc([1u8; 32], "A".into(), false).unwrap();
        let lb = leases.alloc([2u8; 32], "B".into(), false).unwrap();
        let session_port = leases.alloc_session_port([1u8;32],[2u8;32]).unwrap();
        let session_addr = SocketAddr::new(config.public_ip, session_port);

        let sock_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

        // B 先 KNOCK 学习
        let mut knock_b = Vec::new();
        data_header_encode(&mut knock_b, &lb.token, FLAG_KNOCK);
        sock_b.send_to(&knock_b, session_addr).await.unwrap();

        // A 发错令牌 DATA 包
        let mut bad_pkt = Vec::new();
        data_header_encode(&mut bad_pkt, &[0u8; 16], FLAG_DATA); // 错令牌
        bad_pkt.extend_from_slice(b"x");
        sock_a.send_to(&bad_pkt, session_addr).await.unwrap();

        // 300ms 内 B 不应收到
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut buf = [0u8; 2048];
        match tokio::time::timeout(
            std::time::Duration::from_millis(300),
            sock_b.recv_from(&mut buf),
        )
        .await
        {
            Err(_) => {} // 期望超时
            Ok(Ok((n, _))) => panic!("错令牌包不该被转发, 实得 {}", n),
            Ok(Err(e)) => panic!("接收错误: {}", e),
        }

        dp.shutdown().await;
    }

    /// 测试:超载荷(1501B)丢弃
    #[tokio::test]
    #[serial]
    async fn data_plane_drops_oversize() {
        let config = RelayConfig::default_for_test();
        let leases = Arc::new(LeaseTable::new(config.data_port_start..config.data_port_end));
        let dp = DataPlane::spawn(&config, leases.clone()).await.unwrap();

        let la = leases.alloc([1u8; 32], "A".into(), false).unwrap();
        let lb = leases.alloc([2u8; 32], "B".into(), false).unwrap();
        let session_port = leases.alloc_session_port([1u8;32],[2u8;32]).unwrap();
        let session_addr = SocketAddr::new(config.public_ip, session_port);

        let sock_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

        // B 先 KNOCK 学习
        let mut knock_b = Vec::new();
        data_header_encode(&mut knock_b, &lb.token, FLAG_KNOCK);
        sock_b.send_to(&knock_b, session_addr).await.unwrap();

        // A 发超载荷 DATA(1501B)
        let mut oversized_pkt = Vec::new();
        data_header_encode(&mut oversized_pkt, &la.token, FLAG_DATA);
        oversized_pkt.extend_from_slice(&[0u8; 1501]); // 超限
        sock_a.send_to(&oversized_pkt, session_addr).await.unwrap();

        // 300ms 内 B 不应收到
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut buf = [0u8; 2048];
        match tokio::time::timeout(
            std::time::Duration::from_millis(300),
            sock_b.recv_from(&mut buf),
        )
        .await
        {
            Err(_) => {} // 期望超时
            Ok(Ok((n, _))) => panic!("超载荷包不该被转发, 实得 {}B", n),
            Ok(Err(e)) => panic!("接收错误: {}", e),
        }

        dp.shutdown().await;
    }

    /// 测试:KNOCK 只学习不转发
    #[tokio::test]
    #[serial]
    async fn knock_only_learning_no_forward() {
        let config = RelayConfig::default_for_test();
        let leases = Arc::new(LeaseTable::new(config.data_port_start..config.data_port_end));
        let dp = DataPlane::spawn(&config, leases.clone()).await.unwrap();

        let la = leases.alloc([1u8; 32], "A".into(), false).unwrap();
        let lb = leases.alloc([2u8; 32], "B".into(), false).unwrap();
        let session_port = leases.alloc_session_port([1u8;32],[2u8;32]).unwrap();
        let session_addr = SocketAddr::new(config.public_ip, session_port);

        let sock_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

        // A KNOCK,B 未 KNOCK
        let mut knock_a = Vec::new();
        data_header_encode(&mut knock_a, &la.token, FLAG_KNOCK);
        sock_a.send_to(&knock_a, session_addr).await.unwrap();

        // A 发 DATA
        let mut data_a = Vec::new();
        data_header_encode(&mut data_a, &la.token, FLAG_DATA);
        data_a.extend_from_slice(b"hello");
        sock_a.send_to(&data_a, session_addr).await.unwrap();

        // 300ms 内 B 不应收到(因为 B 未 KNOCK,学习表里没 B 的地址)
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut buf = [0u8; 2048];
        match tokio::time::timeout(
            std::time::Duration::from_millis(300),
            sock_b.recv_from(&mut buf),
        )
        .await
        {
            Err(_) => {} // 期望超时
            Ok(Ok((n, _))) => panic!("B 未 KNOCK,DATA 不该被转发, 实得 {}B", n),
            Ok(Err(e)) => panic!("接收错误: {}", e),
        }

        dp.shutdown().await;
    }

    /// 测试:GOODBYE_KNOCK 双方离开后回收会话端口
    #[tokio::test]
    #[serial]
    async fn goodbye_knock_recycles_session_port() {
        let config = RelayConfig::default_for_test();
        let leases = Arc::new(LeaseTable::new(config.data_port_start..config.data_port_end));
        let dp = DataPlane::spawn(&config, leases.clone()).await.unwrap();

        // 分配两个设备令牌
        let la = leases.alloc([1u8; 32], "A".into(), false).unwrap();
        let lb = leases.alloc([2u8; 32], "B".into(), false).unwrap();

        // 分配会话端口
        let session_port = leases.alloc_session_port([1u8;32],[2u8;32]).unwrap();
        let relay_ip = config.public_ip;
        let session_addr = SocketAddr::new(relay_ip, session_port);

        // A/B 的模拟 socket
        let sock_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

        // A/B 各发 KNOCK 学习
        let mut knock_a = Vec::new();
        data_header_encode(&mut knock_a, &la.token, FLAG_KNOCK);
        sock_a.send_to(&knock_a, session_addr).await.unwrap();

        let mut knock_b = Vec::new();
        data_header_encode(&mut knock_b, &lb.token, FLAG_KNOCK);
        sock_b.send_to(&knock_b, session_addr).await.unwrap();

        // 稍等学习生效
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // 断言:会话端口在集合中
        assert!(leases.is_session_port(session_port));

        // A 发 GOODBYE_KNOCK
        let mut goodbye_a = Vec::new();
        data_header_encode(&mut goodbye_a, &la.token, FLAG_GOODBYE_KNOCK);
        sock_a.send_to(&goodbye_a, session_addr).await.unwrap();

        // 稍等处理生效
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // A 离开后端口仍应在(因为 B 还在)
        assert!(leases.is_session_port(session_port));

        // B 发 GOODBYE_KNOCK
        let mut goodbye_b = Vec::new();
        data_header_encode(&mut goodbye_b, &lb.token, FLAG_GOODBYE_KNOCK);
        sock_b.send_to(&goodbye_b, session_addr).await.unwrap();

        // 稍等处理生效
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // 双方都离开后,端口应已回收
        assert!(!leases.is_session_port(session_port));

        // 再次分配会话端口,应能拿到同一端口(证明已回收)
        let new_session_port = leases.alloc_session_port([1u8;32],[2u8;32]).unwrap();
        assert_eq!(session_port, new_session_port);

        dp.shutdown().await;
    }

    /// 测试:NAT 漂移重学习——DATA 包源地址变化时,学习表跟随更新,
    /// 后续对端回包转发到新地址(旧实现转发到死地址,A 新 socket 收不到)
    #[tokio::test]
    #[serial]
    async fn data_relearning_follows_nat_drift() {
        let config = RelayConfig::default_for_test();
        let leases = Arc::new(LeaseTable::new(config.data_port_start..config.data_port_end));
        let dp = DataPlane::spawn(&config, leases.clone()).await.unwrap();

        let la = leases.alloc([1u8; 32], "A".into(), false).unwrap();
        let lb = leases.alloc([2u8; 32], "B".into(), false).unwrap();
        let session_port = leases.alloc_session_port([1u8;32],[2u8;32]).unwrap();
        let session_addr = SocketAddr::new(config.public_ip, session_port);

        // A 的旧地址(sock_a1)与漂移后新地址(sock_a2),B 的 socket
        let sock_a1 = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sock_a2 = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

        // A 自旧地址 KNOCK,B 也 KNOCK(学习表建立)
        let mut knock_a = Vec::new();
        data_header_encode(&mut knock_a, &la.token, FLAG_KNOCK);
        sock_a1.send_to(&knock_a, session_addr).await.unwrap();
        let mut knock_b = Vec::new();
        data_header_encode(&mut knock_b, &lb.token, FLAG_KNOCK);
        sock_b.send_to(&knock_b, session_addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // A 自【新地址】发 DATA(模拟 NAT 重绑定)
        let mut data_a = Vec::new();
        data_header_encode(&mut data_a, &la.token, FLAG_DATA);
        data_a.extend_from_slice(b"drift");
        sock_a2.send_to(&data_a, session_addr).await.unwrap();

        // B 应回到 "drift"(转发路径本身不依赖 A 的地址)
        let mut buf = [0u8; 2048];
        let (n, _) = sock_b.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"drift");

        // B 回 DATA —— 断言转发目的已漂移到 A 的新地址 sock_a2
        // (旧实现转发到 sock_a1,sock_a2 在 300ms 内收不到 → 红)
        let mut data_b = Vec::new();
        data_header_encode(&mut data_b, &lb.token, FLAG_DATA);
        data_b.extend_from_slice(b"back");
        sock_b.send_to(&data_b, session_addr).await.unwrap();

        match tokio::time::timeout(
            std::time::Duration::from_millis(300),
            sock_a2.recv_from(&mut buf),
        ).await {
            Ok(Ok((n, _))) => assert_eq!(&buf[..n], b"back"),
            Ok(Err(e)) => panic!("接收错误: {}", e),
            Err(_) => panic!("NAT 漂移后学习表未更新: 回包仍发往旧地址"),
        }

        dp.shutdown().await;
    }
}
