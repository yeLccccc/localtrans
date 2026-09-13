// 回环吞吐基准测试
// 测试本地进程内 server + client 的传输性能
// 目标：≥150 MB/s (Windows 回环口径, R11 裁决)
//
// 运行: cargo run --release -p localtrans-core --example bench_loopback

use localtrans_core::*;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::time::timeout;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("localtrans_core=debug".parse().unwrap())
        )
        .try_init();

    println!("🚀 启动回环吞吐基准测试...");
    println!("目标: ≥150 MB/s (Windows 回环口径, R11 裁决)\n");

    let total_size = 1024 * 1024 * 1024; // 1GB
    println!("📁 创建 1GB 合成测试文件...");

    // 创建临时目录
    let tmp_dir = TempDir::new()?;
    let server_dir = TempDir::new()?;
    let client_dir = TempDir::new()?;

    // 创建1GB测试文件（分块写入，避免一次性malloc）
    let test_file_path = tmp_dir.path().join("benchmark_1gb.bin");
    create_test_file(&test_file_path, total_size).await?;

    println!("✅ 测试文件创建完成\n");

    // 设置服务器端
    println!("🔧 设置服务器端...");
    let (sm_server, _ev_server, ctx_server, fp_server, _dir_server) =
        test_support::setup_ctx("服务器");

    let server_shares = server_dir.path().join("shares");
    std::fs::create_dir_all(&server_shares)?;
    std::fs::copy(&test_file_path, server_shares.join("benchmark.bin"))?;

    let share_registry = std::sync::Arc::new(share::ShareRegistry::new(vec![
        store::ShareDef {
            id: "bench_share".to_string(),
            alias: "基准测试共享".to_string(),
            path: server_shares,
        },
    ]));

    // 启动服务器监听（随机端口）
    let server_addr = test_support::start_listener(&sm_server).await;
    println!("✅ 服务器端启动: {}\n", server_addr);

    // 设置客户端
    println!("🔧 设置客户端...");
    let (sm_client, _ev_client, ctx_client, fp_client, _dir_client) =
        test_support::setup_ctx("客户端");

    let client_download_dir = client_dir.path().join("downloads");
    std::fs::create_dir_all(&client_download_dir)?;
    ctx_client.config.write().await.download_dir = client_download_dir.clone();

    // 建立互信关系
    println!("🤝 建立互信关系...");
    {
        let mut trust = ctx_server.trust.lock().await;
        trust.upsert(identity::TrustedPeer {
            fingerprint: fp_client,
            name: "客户端".to_string(),
            alias: String::new(),
            paired_at: 1000,
            perms: identity::Perms {
                browse: true,
                download: true,
                push: identity::PushPolicy::Auto,
            },
        });
        trust.save()?;
    }
    {
        let mut trust = ctx_client.trust.lock().await;
        trust.upsert(identity::TrustedPeer {
            fingerprint: fp_server,
            name: "服务器".to_string(),
            alias: String::new(),
            paired_at: 1000,
            perms: identity::Perms {
                browse: true,
                download: true,
                push: identity::PushPolicy::Auto,
            },
        });
        trust.save()?;
    }
    println!("✅ 互信关系建立完成\n");

    // 启动服务器RPC路由器
    let ctrl_rx = sm_server.take_inbound_ctrl_rx().await
        .expect("入站通道未被占用");
    let (ask_tx, _ask_rx) = tokio::sync::mpsc::channel(8);
    transfer::spawn_rpc_router(
        sm_server.clone(),
        ctx_server.clone(),
        share_registry.clone(),
        ctrl_rx,
        ask_tx,
        tokio::sync::mpsc::channel(8).0,
        transfer::sender_state::new_sender_job_map(),
        None,
    );

    // 客户端连接服务器
    println!("🔗 客户端连接服务器...");
    let peer_fp: crate::identity::Fingerprint = timeout(Duration::from_secs(5), sm_client.connect(server_addr))
        .await
        .expect("连接应在超时前完成")?;

    println!("✅ 连接建立成功\n");

    // 开始基准测试
    println!("🚀 开始传输基准测试...");
    println!("文件大小: 1 GB ({} 字节)", total_size);

    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<transfer::ProgressEvent>(64);
    let client_config = ctx_client.config.read().await.clone();
    let reg_unused = share::ShareRegistry::new(vec![]);

    let start_time = Instant::now();

    // 关键：start_pull 必须与进度消费并发跑。
    // 进度通道容量 64，1GB/4MB=256 块的 ChunkDone 事件若无人消费，
    // progress.send().await 反压会卡死接收循环 → 连接静默 → 空闲超时。
    // （首版基准把 start_pull 同步 await 到完成，正是 30s "connection lost" 的根因。）
    let pull_sm = sm_client.clone();
    let pull_handle = tokio::spawn(async move {
        transfer::start_pull(
            &pull_sm,
            &reg_unused,
            &peer_fp,
            "bench_share",
            "benchmark.bin",
            &client_config,
            progress_tx,
        )
        .await
    });

    // 并发监听进度事件
    let mut job_started = false;
    let mut job_done = false;
    let mut total_chunks = 0u32;
    let mut stream_samples = Vec::new();

    // 模拟流数采样（实际应用中会从 AdaptiveStreams 获取）
    let sample_start = Instant::now();

    while let Some(ev) = timeout(Duration::from_secs(60), progress_rx.recv()).await? {
        match ev {
            transfer::ProgressEvent::Started { job_id: j, name, total } => {
                println!("📥 任务开始: ID={}, 名称={}, 大小={} 字节", j, name, total);
                job_started = true;
            }
            transfer::ProgressEvent::ChunkDone { job_id: _j, chunk: _c, bytes: _b } => {
                total_chunks += 1;
                if total_chunks % 16 == 0 {
                    print!(".");
                }
            }
            transfer::ProgressEvent::Speed { job_id: _j, bps } => {
                let elapsed = sample_start.elapsed().as_secs_f64();
                if elapsed > 0.5 { // 每0.5秒采样一次
                    let stream_count = ((bps as f64) / (1_000_000.0)) as u32; // 简化估算
                    stream_samples.push((elapsed, stream_count));
                }
            }
            transfer::ProgressEvent::Done { job_id: j } => {
                println!("\n✅ 任务完成: ID={}", j);
                job_done = true;
                break;
            }
            transfer::ProgressEvent::Failed { job_id: j, reason } => {
                println!("\n❌ 任务失败: ID={}, 原因={}", j, reason);
                return Err(format!("传输失败: {}", reason).into());
            }
            _ => {}
        }
    }

    // 与上面进度事件的 60s 口径一致：若网络慢导致事件间隔逼近上限，
    // 收尾也不应比事件更早判死
    let job_id = timeout(Duration::from_secs(60), pull_handle)
        .await
        .map_err(|_| "拉取任务收尾超时")?
        .map_err(|e| format!("拉取任务 panic: {}", e))?
        .expect("拉取应成功");

    println!("📊 任务ID: {}\n", job_id);
    assert!(job_started, "应收到 Started 事件");
    assert!(job_done, "应收到 Done 事件");

    let elapsed = start_time.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();

    println!("\n📈 基准测试结果:");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("传输大小:    {:.2} MB", total_size as f64 / (1024.0 * 1024.0));
    println!("传输时间:    {:.2} 秒", elapsed_secs);
    println!("平均吞吐:    {:.2} MB/s", total_size as f64 / (1024.0 * 1024.0) / elapsed_secs);
    println!("块总数:      {}", total_chunks);
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // 流数轨迹
    if !stream_samples.is_empty() {
        println!("\n流数轨迹采样:");
        for (time, streams) in stream_samples.iter().take(10) {
            println!("  {:.2}s: ~{} 流", time, streams);
        }
        if stream_samples.len() > 10 {
            println!("  ... (共 {} 个采样)", stream_samples.len());
        }
    }

    // 验证结果
    // R11（SDD 裁决）: 原断言 ≥300MB/s 按 Linux 回环假设制定。实测 Windows 上
    // 1200B（QUIC MTU）UDP 数据报的回环发送天花板 ≈166MB/s（裸 socket 速率），
    // 引擎实测 ~200MB/s 已达裸速率的 ~120%。真实目标千兆以太网上限 112MB/s，
    // 引擎留有 ~80% 余量。故 Windows 回环断言降为 ≥150MB/s。
    let throughput_mb_s = total_size as f64 / (1024.0 * 1024.0) / elapsed_secs;
    println!("\n🎯 验收标准:");
    if throughput_mb_s >= 150.0 {
        println!("✅ PASS: {:.2} MB/s ≥ 150 MB/s (Windows 回环口径, 见 R11 裁决)", throughput_mb_s);
    } else {
        println!("❌ FAIL: {:.2} MB/s < 150 MB/s", throughput_mb_s);
        println!("💡 提示: 低于下限可能原因:");
        println!("   - 缓冲池回收不够高效");
        println!("   - 流并发未充分利用");
        println!("   - 磁盘写入带宽不足（part 文件所在盘）");
        println!("   - 系统资源限制");
    }

    // 验证文件完整性
    let downloaded_path = client_download_dir.join("benchmark.bin");
    if downloaded_path.exists() {
        let original_hash = calculate_file_hash_sync(&test_file_path);
        let downloaded_hash = calculate_file_hash_sync(&downloaded_path);

        if original_hash == downloaded_hash {
            println!("✅ 文件完整性验证通过");
        } else {
            println!("❌ 文件完整性验证失败");
            println!("原始哈希: {}", original_hash);
            println!("下载哈希: {}", downloaded_hash);
        }
    } else {
        println!("❌ 下载文件不存在");
    }

    println!("\n🧹 清理临时文件...");
    // TempDir 会在 drop 时自动清理

    Ok(())
}

// 创建测试文件（分块写入避免一次malloc大内存）
async fn create_test_file(path: &std::path::Path, size: usize) -> std::io::Result<()> {
    use tokio::fs::File;
    use tokio::io::{AsyncWriteExt, BufWriter};

    let file = File::create(path).await?;
    let mut writer = BufWriter::new(file);
    const CHUNK_SIZE: usize = 1024 * 1024; // 1MB chunks

    let mut written = 0;
    while written < size {
        let remaining = size - written;
        let chunk_size = std::cmp::min(CHUNK_SIZE, remaining);

        // 生成伪随机数据（简单模式：用计数器）
        let chunk: Vec<u8> = (0..chunk_size).map(|i| (i % 251) as u8).collect();

        writer.write_all(&chunk).await?;
        written += chunk_size;

        if written % (100 * 1024 * 1024) == 0 { // 每100MB输出一次
            println!("  已写入 {} MB", written / (1024 * 1024));
        }
    }

    writer.flush().await?;
    Ok(())
}

// 计算文件SHA256哈希（同步版本，避免 Send 问题）
fn calculate_file_hash_sync(path: &std::path::Path) -> String {
    use sha2::{Sha256, Digest};
    use std::io::Read;

    let mut file = std::fs::File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let n = file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    format!("{:x}", hasher.finalize())
}