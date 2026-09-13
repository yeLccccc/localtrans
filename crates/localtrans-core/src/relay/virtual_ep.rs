//! 虚拟端点:quinn Endpoint 跑在自定义 AsyncUdpSocket 上。
//! 发包:18B 头 + 载荷 → 中继会话端口;收包:剥头 → 交给 quinn。
//! 逻辑地址:发到中继会话地址的所有包,quinn 看到的"对端"。

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, Endpoint, UdpPoller};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::net::UdpSocket;
use tokio::io::ReadBuf;

use super::proto::{data_header_decode, data_header_encode, DATA_HEADER_LEN, FLAG_DATA};

/// 虚拟 UDP socket:所有出站包加 18B 头,所有入站包剥头。
#[derive(Debug)]
pub struct VirtualUdp {
    sock: Arc<UdpSocket>,
    /// 中继会话地址(所有出包目的地;所有入包宣称来源)
    relay_session_addr: SocketAddr,
    token: [u8; 16],
}

impl VirtualUdp {
    /// 绑定任意本地端口,连通中继会话端口。
    /// 返回 Arc<Self> 以便与 quinn 共享。
    pub async fn bind(token: [u8; 16], relay_session_addr: SocketAddr) -> io::Result<Arc<Self>> {
        let bind_addr: SocketAddr = if relay_session_addr.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };
        let sock = UdpSocket::bind(bind_addr).await?;

        // connect() 设置默认目标,过滤入包
        sock.connect(relay_session_addr).await?;
        tracing::debug!("VirtualUdp 绑定并 connect 到 {}", relay_session_addr);
        Ok(Arc::new(Self {
            sock: Arc::new(sock),
            relay_session_addr,
            token,
        }))
    }

    /// 从**本 socket** 发 KNOCK(让中继学习到本端真实地址)。
    /// 必须由 VirtualUdp 自己发——若用临时 socket 发,中继学到的是临时端口,
    /// 后续转发的包会全部发往那个已丢弃的端口(E2E 实测踩过这个坑)。
    pub async fn send_knock(&self) -> io::Result<()> {
        use crate::relay::proto::{data_header_encode, FLAG_KNOCK};
        let mut pkt = Vec::with_capacity(crate::relay::proto::DATA_HEADER_LEN + 1);
        data_header_encode(&mut pkt, &self.token, FLAG_KNOCK);
        pkt.push(0); // KNOCK 载荷 1B
        self.sock.send(&pkt).await.map(|_| ())
    }
}

/// UdpPoller:恒 Ready —— quinn 会循环 poll_writable,我们的 try_send 是非阻塞,
/// 失败丢包给 QUIC 重传兜底(UDP 本来就允许丢;局域网/公网 send buffer 满的概率极低)。
#[derive(Debug, Default)]
struct TokioPoller;

impl UdpPoller for TokioPoller {
    fn poll_writable(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncUdpSocket for VirtualUdp {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(TokioPoller::default())
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        // 只允许发往中继会话地址(其余目标说明 quinn 试图响应其他地址——丢弃)
        if transmit.destination != self.relay_session_addr {
            return Ok(()); // 静默丢弃,不报错(quinn 会重试)
        }
        let mut pkt = Vec::with_capacity(DATA_HEADER_LEN + transmit.contents.len());
        data_header_encode(&mut pkt, &self.token, FLAG_DATA);
        pkt.extend_from_slice(transmit.contents);
        match self.sock.try_send(&pkt) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // 发送缓冲区满,静默丢包(QUIC 会重传)
                tracing::trace!("VirtualUdp 缓冲区满,丢包(QUIC 会重传)");
                Ok(())  // 返回 OK 而不是 WouldBlock,避免 QUIC 紧密重试
            },
            Err(e) => {
                tracing::trace!("VirtualUdp 发送失败: {}", e);
                Ok(())  // 静默丢包
            },
        }
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [std::io::IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        // 单包收:剥 18B 头,源地址统一宣称中继会话地址
        let mut tmp = [0u8; 2048];
        let mut read_buf = ReadBuf::new(&mut tmp);
        match self.sock.poll_recv_from(cx, &mut read_buf) {
            Poll::Ready(Ok(_addr)) => {
                let n = read_buf.filled().len();
                // 中继转发来的包是**裸载荷**(中继已剥 18B 头);
                // 只有极少数直达包才带自己的头。判定法:
                // 头 18B 合法且 token 匹配 → 剥头;否则整体视为裸载荷交付。
                let payload: &[u8] = if n >= DATA_HEADER_LEN {
                    match data_header_decode(&tmp[..n]) {
                        Ok((token, _flag)) if token == self.token => &tmp[DATA_HEADER_LEN..n],
                        _ => &tmp[..n],
                    }
                } else {
                    &tmp[..n]
                };
                if payload.is_empty() {
                    return Poll::Ready(Ok(0));
                }
                bufs[0][..payload.len()].copy_from_slice(payload);
                meta[0] = RecvMeta {
                    addr: self.relay_session_addr,
                    len: payload.len(),
                    stride: payload.len(),
                    ecn: None,
                    dst_ip: None,
                };
                Poll::Ready(Ok(1))
            },
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        1
    }

    fn max_receive_segments(&self) -> usize {
        1
    }

    fn may_fragment(&self) -> bool {
        false
    }
}

use std::pin::Pin;

// 中继内层端点传输配置在 session::relay_transport_config(与局域网
// base_transport_config 同源,仅 idle 60s→15s;见该函数注释)。

/// 从虚拟 socket 建 quinn Endpoint(服务端模式:接受内层 QUIC 连接)。
/// 虚拟端点复用:不影响局域网行为
pub async fn server_endpoint(vudp: Arc<VirtualUdp>, id: &crate::identity::Identity) -> Result<Endpoint, String> {
    let server_cfg = crate::session::server_config(id)
        .map_err(|e| format!("server_config: {}", e))?;
    // 覆写 idle 15s(见 relay_transport_config 注释)
    let mut server_cfg = server_cfg;
    server_cfg.transport_config(crate::session::relay_transport_config());
    let mut ep_cfg = quinn::EndpointConfig::default();
    // MTU 降到 1200:避开中间设备(MTU 黑洞/PPPoE/VPN)的 PMTU 异常,
    // 减少 quinn sendmsg EMSGSIZE 与重传,改善跨网传输抖动
    ep_cfg.max_udp_payload_size(1200).map_err(|e| format!("max_udp_payload_size: {}", e))?;
    let ep = Endpoint::new_with_abstract_socket(
        ep_cfg,
        Some(server_cfg),
        vudp,
        Arc::new(quinn::TokioRuntime),
    ).map_err(|e| e.to_string())?;
    Ok(ep)
}

/// S1: 带客户端证书钉扎的服务端端点(中继 accept_peer 用)。
/// expected_fp = PunchNotif.from_fp:TLS 握手期即比对客户端证书指纹,
/// 不符握手失败——消除"握手完成后才比对"的验证窗口。
pub async fn server_endpoint_pinned(
    vudp: Arc<VirtualUdp>,
    id: &crate::identity::Identity,
    expected_fp: [u8; 32],
) -> Result<Endpoint, String> {
    let server_cfg = crate::session::server_config_pinned(id, expected_fp)
        .map_err(|e| format!("server_config_pinned: {}", e))?;
    let mut server_cfg = server_cfg;
    server_cfg.transport_config(crate::session::relay_transport_config());
    let mut ep_cfg = quinn::EndpointConfig::default();
    ep_cfg.max_udp_payload_size(1200).map_err(|e| format!("max_udp_payload_size: {}", e))?;
    let ep = Endpoint::new_with_abstract_socket(
        ep_cfg,
        Some(server_cfg),
        vudp,
        Arc::new(quinn::TokioRuntime),
    ).map_err(|e| e.to_string())?;
    Ok(ep)
}

/// 客户端模式(connect 用)。
/// 虚拟端点复用:不影响局域网行为
///
/// 附带自签客户端证书(与局域网 connect 的 mTLS 一致)——否则对端
/// server_endpoint 的 AcceptAnySelfSignedClientCert 收不到任何证书,
/// SessionManager::handle_incoming 会 NoPeerCert(v0.4.0 同意门 E2E 发现)。
pub async fn client_endpoint(vudp: Arc<VirtualUdp>, id: &crate::identity::Identity, expected_fp: Option<[u8; 32]>) -> Result<Endpoint, String> {
    let mut ep_cfg = quinn::EndpointConfig::default();
    ep_cfg.max_udp_payload_size(1200).map_err(|e| format!("max_udp_payload_size: {}", e))?;
    let mut ep = Endpoint::new_with_abstract_socket(
        ep_cfg,
        None,
        vudp,
        Arc::new(quinn::TokioRuntime),
    ).map_err(|e| e.to_string())?;
    let rustls_cfg = crate::session::client_builder(expected_fp)
        .map_err(|e| format!("client_builder: {}", e))?
        .with_client_auth_cert(
            vec![id.cert.clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(id.pkcs8.clone().into()),
        )
        .map_err(|e| format!("with_client_auth_cert: {}", e))?;
    let quic_cfg = quinn::crypto::rustls::QuicClientConfig::try_from(rustls_cfg)
        .map_err(|e| format!("QuicClientConfig: {}", e))?;
    let mut client_cfg = quinn::ClientConfig::new(Arc::new(quic_cfg));
    client_cfg.transport_config(crate::session::relay_transport_config());
    ep.set_default_client_config(client_cfg);
    Ok(ep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::proto::{data_header_encode, FLAG_DATA, FLAG_KNOCK};
    use std::time::Duration;

    /// 测试:VirtualUdp 的 18B 头处理(构造两个真 UdpSocket 对拍)
    #[tokio::test]
    async fn virtual_udp_strips_header() {
        // 中继 socket(扮演服务端)
        let relay_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay_sock.local_addr().unwrap();

        // 客户端 VirtualUdp
        let token = [1u8; 16];
        let vudp = VirtualUdp::bind(token, relay_addr).await.unwrap();

        // 模拟中继发一个带头的包
        let mut pkt = Vec::new();
        data_header_encode(&mut pkt, &token, FLAG_DATA);
        pkt.extend_from_slice(b"hello quinn");

        relay_sock.send_to(&pkt, vudp.local_addr().unwrap()).await.unwrap();

        // 等待数据到达
        tokio::time::sleep(Duration::from_millis(10)).await;

        // poll_recv 应剥头返回 "hello quinn"
        let mut buf = [0u8; 1024];
        let mut bufs = [std::io::IoSliceMut::new(&mut buf)];
        let mut meta = [RecvMeta::default()];
        let mut cx = Context::from_waker(std::task::Waker::noop());
        let n = vudp.poll_recv(&mut cx, &mut bufs, &mut meta);
        match n {
            Poll::Ready(Ok(1)) => {
                assert_eq!(&bufs[0][..meta[0].len], b"hello quinn");
                assert_eq!(meta[0].addr, relay_addr);
            }
            other => panic!("期望 Ready(Ok(1)), 实得 {:?}", other),
        }
    }

    /// 测试:错令牌头包按裸载荷整体交付(中继转发的包本就无头,
    /// 收端无法可靠区分"坏头"和"裸载荷"——裸载荷是正常形态)
    #[tokio::test]
    async fn virtual_udp_drops_wrong_token() {
        let relay_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay_sock.local_addr().unwrap();

        let token = [1u8; 16];
        let vudp = VirtualUdp::bind(token, relay_addr).await.unwrap();

        // 发错令牌包
        let mut pkt = Vec::new();
        data_header_encode(&mut pkt, &[2u8; 16], FLAG_DATA);
        pkt.extend_from_slice(b"x");
        relay_sock.send_to(&pkt, vudp.local_addr().unwrap()).await.unwrap();

        // 稍等让包到达
        tokio::time::sleep(Duration::from_millis(10)).await;

        let mut buf = [0u8; 1024];
        let mut bufs = [std::io::IoSliceMut::new(&mut buf)];
        let mut meta = [RecvMeta::default()];
        let mut cx = Context::from_waker(std::task::Waker::noop());
        // 新语义:头不合法(token 不匹配)→ 整包作为裸载荷交付
        match vudp.poll_recv(&mut cx, &mut bufs, &mut meta) {
            Poll::Ready(Ok(1)) => {
                // 交付的是整包(18B 头 + "x")
                assert_eq!(meta[0].len, crate::relay::proto::DATA_HEADER_LEN + 1);
            }
            other => panic!("错令牌头包应按裸载荷交付, 实得 {:?}", other),
        }
    }

    /// 测试:短包(<18B)按裸载荷交付
    #[tokio::test]
    async fn virtual_udp_drops_short_packet() {
        let relay_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay_sock.local_addr().unwrap();

        let token = [1u8; 16];
        let vudp = VirtualUdp::bind(token, relay_addr).await.unwrap();

        // 发短包
        relay_sock.send_to(b"short", vudp.local_addr().unwrap()).await.unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;

        let mut buf = [0u8; 1024];
        let mut bufs = [std::io::IoSliceMut::new(&mut buf)];
        let mut meta = [RecvMeta::default()];
        let mut cx = Context::from_waker(std::task::Waker::noop());
        match vudp.poll_recv(&mut cx, &mut bufs, &mut meta) {
            Poll::Ready(Ok(1)) => {
                assert_eq!(meta[0].len, 5); // "short" 整体交付
            }
            other => panic!("短包应按裸载荷交付, 实得 {:?}", other),
        }
    }

    /// 测试:自己 token 的 KNOCK 头包剥头后载荷为空 → Ok(0)
    /// (中继对 KNOCK 只学习不转发,这种包到达属异常)
    #[tokio::test]
    async fn virtual_udp_drops_knock_flag() {
        let relay_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay_sock.local_addr().unwrap();

        let token = [1u8; 16];
        let vudp = VirtualUdp::bind(token, relay_addr).await.unwrap();

        // 发 KNOCK 包
        let mut pkt = Vec::new();
        data_header_encode(&mut pkt, &token, FLAG_KNOCK);
        relay_sock.send_to(&pkt, vudp.local_addr().unwrap()).await.unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;

        let mut buf = [0u8; 1024];
        let mut bufs = [std::io::IoSliceMut::new(&mut buf)];
        let mut meta = [RecvMeta::default()];
        let mut cx = Context::from_waker(std::task::Waker::noop());
        match vudp.poll_recv(&mut cx, &mut bufs, &mut meta) {
            Poll::Ready(Ok(0)) => {} // 剥头后载荷空
            other => panic!("空载荷 KNOCK 应返回 Ok(0), 实得 {:?}", other),
        }
    }
}
