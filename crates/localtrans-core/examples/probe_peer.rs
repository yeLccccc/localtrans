// 对端发现探针（诊断工具）
//
// 用法: cargo run --release -p localtrans-core --example probe_peer -- <对端IP> [本地端口]
//
// 从独立端口向对端的发现端口(默认 47600,可用 LOCALTRANS_TEST_PORT_BASE
// 偏移)发探测包，并把收到的每个包
// 连同校验结果、时钟偏差一起打印——用于定位"互相看不见"到底断在哪一层:
//   - 发了没回     → 对端没开软件 / 对端防火墙 / 网络丢弃
//   - 回了但 BadTs → 两台机器系统时间差超过 ±30 秒
//   - 回了但正常   → 发现链路本身没问题，问题在应用层

use ed25519_dalek::SigningKey;
use localtrans_core::discovery::{encode_and_sign, verify_and_parse, DiscoveryPacket, PacketKind, ReplayGuard};
use rand::rngs::OsRng;
use rand::RngCore;
use std::net::UdpSocket;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn main() {
    let target_ip = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("用法: probe_peer <对端IP> [本地端口]");
        std::process::exit(2);
    });
    let local_port: u16 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(47700); // 避开正在运行的应用占用的默认发现端口

    let target: std::net::SocketAddr =
        format!("{}:{}", target_ip, localtrans_core::ports::discovery_port()).parse().expect("对端地址无效");
    let sock = UdpSocket::bind(format!("0.0.0.0:{}", local_port)).expect("绑定本地端口失败");
    println!("[探针] 本地 {} -> 目标 {}", sock.local_addr().unwrap(), target);
    println!("[探针] 本机当前时间戳: {} ({}", now_ms(),
        humantime(now_ms()));
    println!();

    // 临时身份（不影响双方真实设备表——用完即弃，最多在对端留一条 30 秒后过期的记录）
    let key = SigningKey::generate(&mut OsRng);
    let fp: [u8; 32] = key.verifying_key().to_bytes(); // 用公钥当指纹，格式与真实指纹一致

    let pkt = DiscoveryPacket {
        v: 1,
        kind: PacketKind::Probe,
        name: "诊断探针".into(),
        fingerprint: fp,
        pubkey: fp,
        quic_port: localtrans_core::ports::quic_port(),
        ts_ms: now_ms(),
        nonce: {
            let mut n = [0u8; 12];
            OsRng.fill_bytes(&mut n);
            n
        },
    };
    let wire = encode_and_sign(&pkt, &key);
    println!("[发送] Probe 包 {} 字节 (签名/时间戳已附)", wire.len());

    // 发 3 次、每次间隔 1 秒；接收循环总共跑 15 秒
    sock.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    let start = std::time::Instant::now();
    let mut replay = ReplayGuard::new();
    let mut sent = 0;
    let mut got_any = false;

    while start.elapsed() < Duration::from_secs(15) {
        if sent < 3 && start.elapsed().as_secs() >= sent as u64 {
            match sock.send_to(&wire, target) {
                Ok(n) => println!("[发送] 第 {} 次已发出 ({} 字节)", sent + 1, n),
                Err(e) => println!("[发送] 第 {} 次失败: {}", sent + 1, e),
            }
            sent += 1;
        }

        let mut buf = [0u8; 4096];
        match sock.recv_from(&mut buf) {
            Ok((len, src)) => {
                got_any = true;
                let recv_at = now_ms();
                match verify_and_parse(&buf[..len], recv_at, &mut replay) {
                    Ok(p) => {
                        let skew = recv_at as i64 - p.ts_ms as i64;
                        println!("[收到] {} 字节来自 {}", len, src);
                        println!("       类型={:?} 名称={} 指纹前8={} quic_port={}",
                            p.kind, p.name,
                            p.fingerprint.iter().take(4)
                                .map(|b| format!("{:02x}", b)).collect::<String>(),
                            p.quic_port);
                        println!("       对端时间戳={} ({})  时钟偏差={:+} 秒 {}",
                            p.ts_ms, humantime(p.ts_ms),
                            skew as f64 / 1000.0,
                            if skew.abs() > 30_000 { "<-- 超出±30s窗口，会被正式服务拒绝!" } else { "(窗口内)" });
                    }
                    Err(e) => {
                        println!("[收到] {} 字节来自 {}  但校验失败: {:?}", len, src, e);
                    }
                }
            }
            Err(_) => { /* 超时轮询，继续 */ }
        }
    }

    println!();
    if got_any {
        println!("[结论] 对端有回应——发现链路通。若应用里仍看不到对方，问题在应用层，带着日志再查。");
    } else {
        println!("[结论] 15 秒内无任何回应。可能: 对端 LocalTrans 没在运行 / 对端隐身 / 网络丢弃。");
    }
}

fn humantime(ms: u64) -> String {
    // 不引 chrono，直接给 UTC 小时分秒（够用了，看偏差就行）
    let secs = ms / 1000;
    format!("UTC {:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}
