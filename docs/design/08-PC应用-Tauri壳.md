# 08 PC 应用 — Tauri 壳

> 源码:`src-tauri/src/`(main.rs 1686 行 / commands.rs / events.rs / firewall.rs)、tauri.conf.json。前端见 09 篇。

## 1. 技术选型与产物

- Tauri 2 + tauri-plugin-{single-instance, dialog, opener, notification};windows-sys(MessageBoxW)。
- **便携 exe**(bundle.active=false):`target/release/localtrans.exe`(工作区根 target),数据全部在 exe 旁 `data/`。
- 单窗口 1000×680,系统标题栏,无托盘;frontendDist = `../ui/dist`(Vue 构建产物嵌入)。
- CSP:`default-src 'self'; connect-src 'self' ipc: http://ipc.localhost; img-src 'self' data:; style-src 'self' 'unsafe-inline'`。

## 2. 启动流程(main.rs)

```
1. 日志:data/logs/localtrans.log.YYYY-MM-DD 按天滚动(LOCALTRANS_LOG 控级别)
2. WebView2 检测(注册表缺失 → MessageBox 提示退出)
3. 插件注册(single-instance 二次启动 focus 主窗)
4. setup(block_on):
   identity.load_or_create → load_config → TrustStore.load
   → discovery::spawn → SessionManager::spawn → start_listener(47601)
   → ShareRegistry::new(config.shares) → spawn_rpc_router(全部 ask/delete/auto/recv/source 通道)
   → AppState 构造 + manage
   → 中继启动(relay_enabled 且校验过) + 事件桥
   → 共享 watchdog(2s,变化 → emit shares-changed + 对连接对端发 SharesChanged)
   → 防火墙自检(缺失 → toast warning)
   → 各事件泵(见 §3)
5. 启动迁移:active/pending/paused → interrupted(push 或有 parts 保留);gc_stale_parts(7天/全真)
```

## 3. 事件桥(核心事件 → Tauri emit)

| 泵 | 消费 | 产出 |
|---|---|---|
| 会话事件 | SessionEvent 全部 | pairing-* 5 事件;PairingResult ok → connected_fps+device-list+connection-state;SessionUp → **回路 2 自动续传**(failed+parts 任务 resume_pending,每任务 1 次);SessionDown → **回路 1 中继自愈**(本地无+名册有 → peer-reconnecting → auto_reconnect 退避 3 败) |
| 发现 watch | devices watch | merge(本地+名册+connected+别名+信任)→ device-list |
| 接收泵 recv_rx | ProgressEvent | Started/Resumed/ChunkDone/Done/Failed/InstantHit → transfers 表;Done 且 Auto 档 → **系统通知**(notification 插件,失败降级 toast);InstantHit → 行直接 done+instant |
| ask_tx 泵 | OfferAsk | pending_offers(oneshot+extend Notify)+deadline+3s 看门狗 → emit offer-request |
| auto_tx 泵 | Auto 档 offer | 记 (peer, 数量) 供 Done 通知 |
| delete_ask 泵 | DeleteAsk | pending_deletes + 看门狗 → emit delete-request |
| source 泵 | Source* 事件 | 落 transfers 表(source-push/source-pull 行) |
| 传输聚合 | 表快照 | **动态频率**:有 open 任务 → 250ms(4Hz)无条件 emit;全终态 → 1Hz + 内容 hash 脏检查跳过 → transfer-progress |
| 持久化 | 脏标记 | 1s tick 原子写 transfers.json(done/failed 封顶 150 条) |

## 4. Tauri Command 全清单(commands.rs,注册于 main.rs L1349-1410)

### 设备
| 命令 | 行为 |
|---|---|
| list_devices | 内存 devices(merge 结果) |
| set_hidden | config 落盘;中继开且翻转 → reconnect_relay |
| probe_now | 广播探测 |
| add_manual_device(addr) | ProbeAddr + 5s 后 emit manual-probe-result |
| connect(fp) | 本地有 → connect_pinned;名册有 → relay.connect_peer + adopt_as_initiator;emit device-list+connection-state |

### 配对
get_pairing_pending / submit_pair_code / reject_pairing(兼容保留) / grant_consent(→own_code) / deny_consent / cancel_pairing_wait

### 浏览/传输
| 命令 | 行为 |
|---|---|
| list_shares_remote / list_dir_remote | send_rpc 6s 超时 + 失败自动重试 1 次 |
| start_download(fp, share_id, path) | 占位任务 + xfer_lock(60s 排队)→ start_pull |
| push_files(fp, local_paths) | 占位 + 锁 → push_files |
| start_download_dir | 聚合占位行"[文件夹] name" → start_pull_dir + DirPullEvent 泵 |
| push_files_rel(fp, items) | 占位 + placeholder_cancels → push_files_rel_cancellable |
| expand_local_paths(paths) | BFS 展开(≤32 深/≤10000 文件/敏感目录黑名单/跳隐藏) |
| prepare_shutdown | 5s 封顶:任务→interrupted → shutdown_all(Goodbye)→ relay.shutdown → abort 泵 → 点燃占位取消 → transfers.json 落盘 |
| respond_offer(job_id, accepted, save_dir?) | pending_offers oneshot;save_dir 仅校验非空+绝对路径(不限目录,BUG06 裁定) |
| offer_extend(job_id) | 顺延"另存中"deadline |
| respond_delete(ask_id, allow) | 删除确认门应答 |
| transfer_action(job_id, action) | 三级路由:占位→placeholder_cancels;sender_jobs;get_push_control;回退 control_task;cancel 时向对端发 TransferCtl::Cancel |
| transfer_throttle(job_id, max_streams) | sender 限速 |

### 队列/历史
list_transfers / pending_resume_jobs / resume_pending(单文件重连+续传;文件夹走 run_dir_pull_task 编排) / clear_completed_transfers / remove_transfer(job_id, delete_parts) / has_parts(job_id)

### 设置
get_settings(relay_psk 掩码留尾 4) / save_settings(改名→SetName 广播+中继重启;超时 clamp 15-600) / add_share / remove_share / list_trusted / set_alias / set_perms / remove_trusted(移除即断,P0-5)

### 中继
set_relay_config(掩码还原→validate→落盘→reconnect/shutdown) / relay_status({enabled,connected,server,devices,error})

### 系统
add_firewall_rule(netsh+UAC) / get_network_status(5s 缓存:local_ip/规则/三 profile) / open_logs_dir / open_download_dir / get_device_fingerprint

## 5. AppState(main.rs L24-84)

dir/identity/config(RwLock)/trust/sm/discovery/reg/hidden + pending_offers(job→{respond oneshot, extend Notify})/auto_offers/pending_deletes/pending_pairing/devices/transfers 表+dirty/next_placeholder_id(u64::MAX 递减)/xfer_lock(传输协商串行)/placeholder_cancels/connected_fps/sender_jobs/relay+roster+任务句柄/healing_fps/auto_retried。

## 6. 防火墙(firewall.rs)

- `add_rule`:`powershell Start-Process netsh -Verb RunAs`(UAC)添加入站 UDP 47600-47601 规则 "LocalTrans";CREATE_NO_WINDOW 防黑框;识别"用户取消"。
- `rule_status`:netsh show + 中英双语解析(exists/enabled)。
- `profile_states`:注册表读三 profile 防火墙开关。
- `primary_local_ip`:UDP connect 8.8.8.8:9 选路取主网卡 IP(不真正发包)。

## 7. 关闭语义(useCloseGuard 配合)

窗口关闭请求 → 有活动任务(active/pending/paused)→ preventDefault 弹确认 → 强制关闭走 `prepare_shutdown()`(Goodbye 通知对端 + 位图落盘)→ win.destroy()。

## 8. 设计要点

- **推模式事件 + 拉模式命令**:数据流全部事件驱动(无轮询);命令只做动作。
- **占位任务模式**:UI 即时看到 pending 行(占位 id),实际 job_id 在引擎 Started 事件后回填 —— 网络慢时不白屏。
- **xfer_lock**:同一时刻只允许一个传输协商(metainfo 协商期间独占),排队 60s 超时落 failed。
- 传输进度 4Hz/1Hz 动态频率:活跃高频,全终态降频省 CPU(常驻 CPU 占用优化的产物)。
