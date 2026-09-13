# localTrans 全仓库安全与稳定性审计报告

日期:2026-08-26 | 基线:v0.10.3(f872bd4 之后,6130f73)
修复状态:v0.11.0 全量修复完毕(2026-08-27,76/76 ✅)——逐项“✅ 已修”标注映射到修复 commit。
方法:6 个并行审查代理按组件×视角全覆盖(core transfer / core session+discovery / relay 服务端 / 协议信任链 / FFI+Tauri 壳 / Android+Vue 前端),控制器对 Critical 与击穿核心承诺的发现逐条亲验代码。去重后 **2 Critical / 18 High / 31 Medium / 25 Low,共 76 条**(原始 82 条,合并 8 条跨代理重复,剔除 1 条误报)。

---

## 一、Critical(Android 崩溃,必先修)

| # | 问题 | 位置 | 触发 |
|---|---|---|---|
| C1 | 指纹 hex 长度≠32 时 `copy_from_slice` panic 跨 FFI = **进程 abort**。`push_files`/`push_files_rel`/`pull_files`/`backup_push` 均为非 Result 导出,uniffi 对 panic 走 abort | localtrans-ffi/lib.rs:1117,1248,1370,1953 | Kotlin 传空串/截断指纹即崩 | ✅ 2918f5f+06ed117 |
| C2 | `start()` 内 5 处 `expect`(QUIC listener/discovery spawn/identity/ctrl_rx 等),`start()` 返回 `()` 非 Result | localtrans-ffi/lib.rs:304,370,405,418,421 | 端口被占(桌面版同机测试)、磁盘满、权限被收回 → abort | ✅ 2918f5f+06ed117 |

修法:抽 `decode_fp32()->Result<[u8;32],AppException>` 统一替换;`start()` 改 `Result<(),AppException>`。同文件 `share_op`/`disconnect` 已有长度检查,证明是遗漏非约定。

## 二、High — 安全(信任链三连,击穿"中继看不到明文")

| # | 问题 | 位置 |
|---|---|---|
| S1 | **内层 QUIC 不做指纹钉扎**:relay 模式 `client_builder(None)`、局域网 `connect(addr)` 也丢弃发现包签名指纹 → 恶意中继/ARP 欺骗者可在**首次配对**时以自签证书分插双方,MITM 指纹进信任表,此后全程明文可见。同意门+6 位码流程完全正常展示,用户无感 | core/relay/virtual_ep.rs:186-195 + session.rs:227-231,787 | ✅ a42f99b+f07a463 |
| S2 | **中继 Register 指纹自声明无占有证明**:任何 PSK 持有者可注册受害者 fp——旧 token 即刻作废(断流)、conns 被顶占(名册/Punch 推给攻击者)、真机断开时清理失灵(劫持长存)。与 S1 组合 = 完整身份替换链 | relay/control.rs:268-279 + lease.rs:71-81 | ✅ d3d8757+f0e0aff |
| S3 | **KNOCK 不验会话成员**:任何 PSK 持有者向端口池全部端口各发一个包即插入他人会话,约半数概率持续收双方密文+随机断流 | relay/data_plane.rs:141-146,190-205 | ✅ 37d7ec7(+e99ce7e) |

修法:S1 = virtual_ep 传 `expected_fp`、accept 侧绑 PunchNotif.from_fp、局域网 connect 用发现包指纹(价值最高,优先);S2 = Register 携带私钥签名(fp+nonce)验占有;S3 = Punch 时登记 `(port,{A,B})` 成员集,KNOCK/DATA 校验成员。

## 三、High — 安全(公网服务瘫痪/资源)

| # | 问题 | 位置 |
|---|---|---|
| S4 | 控制面**持锁跨 await 推送无超时**:恶意客户端不读 uni 流,流控耗尽后 `open_uni` 永久挂起 → conns 锁上所有路径(Ping/Punch/Leave/广播)全部死锁,自打百次 Ping 即瘫全服务 | relay/control.rs:421-430,304-316 | ✅ d3053d2+e99ce7e |
| S5 | 控制帧 4B 长度前缀无上限:`len=0xFFFFFFFF` 即预分配 4GiB,数连接 OOM。encode 侧有 64KB 上限,decode 侧没有 | relay/control.rs:260-264(客户端侧 client.rs:166 同构) | ✅ f0e0aff+d3053d2 |
| S6 | 会话端口只增不减:无 TTL/速率限制,循环 Punch 不 KNOCK 每次泄漏一个端口,池干涸后只能重启恢复 | relay/lease.rs:187-203 + control.rs:330 | ✅ 37d7ec7 |
| S7 | 未认证连接无证明超时+无单 IP/全局连接上限,keep-alive 反向维持占坑;拉黑判定在握手后,被拉黑 IP 仍可烧握手 CPU | relay/control.rs:195-197,182-192 | ✅ d3053d2+e99ce7e |
| S8 | ReplayGuard 先登记 nonce 后验签+容量满全清:无签名 UDP 洪泛即可打穿反重放窗口,重放嗅探包维持离场设备"在线" | core/discovery.rs:76-79,125-155 | ✅ b692245+7d2ef76 |
| S9 | **拉取侧路径穿越**:远端 ListResp 的 `e.name` 拼 `rel` 未经净化直接 join 本地 dest,`../evil` 或 `C:\x` 可逃出下载目录写任意位置(推送侧已修,拉取侧漏) | core/transfer/engine.rs:2708-2717 | ✅ b692245 |

## 四、High — 稳定性(一次触发即永久瘫痪/长时间卡死)

| # | 问题 | 位置 |
|---|---|---|
| T1 | **BufferPool 许可泄漏**:run_sender 网络阶段(open_uni/write/finish)错误路径不 release,`permit.forget()` 设计下约 8 次传输中断即耗尽共享 32 许可 → 此后该进程一切推/拉永久超时 | engine.rs:1550-1561,330-331 | ✅ 86f69d7 |
| T2 | run_receiver_windowed 提前返回 abort 在途块任务,同样泄漏路由器共享池许可 | engine.rs:1495-1509 | ✅ 86f69d7 |
| T3 | **入站响应通道一次性 take 后发送失败不归还**:对端恰好掉线时 `send_ctrl` Err 直接 `?`,`resp_rx` 被 drop → 后续全部 pull/push/list 永远报"通道已被占用",只能重启 | engine.rs:556-564,2608-2614 | ✅ 07ba31a |
| T4 | 路由器内联等用户应答(Ask 档≤600s 可顺延):期间所有对端控制面(含取消)积压/丢弃 | engine.rs:1934-1956 | ✅ ed1b2d1 |
| T5 | 路由器内联 `Manifest::build` 整文件 SHA256:100GB 卡路由器 100s+,idle watchdog 先杀正在进行的其他传输;同族:op_delete/count_dir_entries 同步重 IO | engine.rs:1693-1699 + share.rs:238-245(两个代理独立发现) | ✅ ed1b2d1 |
| T6 | FFI 全方法持全局 std Mutex 期间 block_on 做 10s 网络:`list_remote` 挂起时主线程 `devices()` 排队 → ANR;且 `connect_device` 无超时(对端离线锁 60s) | ffi/lib.rs:840-886,1707-1761 + session.rs:448 | ✅ 80b6ca6 |
| T7 | Android 主线程整树遍历:`file.walk()` 选大目录(DCIM)发送时阻塞主线程数十秒 → ANR | FilesScreen.kt:653-679 | ✅ 1c726dd |

## 五、Medium — 按主题归组(31 条)

**数据泄露专项(用户重点关切)**
- ✅ 已修(6d8135a) M-A1 [High 边缘] `allowBackup` 未关:identity.key 私钥/信任库/PSK 随 Android 云备份上 Google,换机恢复即导出身份 → 冒充本机(AndroidManifest.xml)
- ✅ 已修(6d8135a) M-A2 全量 AppEvent 打 logcat:`PairingCodeShown(code)` 配对码、指纹、文件名、localPath;TransferUpdated 高频还刷日志(LocalTransApp.kt:119)
- ✅ 已修(3fcaaad+a67a1cf) M-A3 CSP 为 null + opener 放行 `**` + `expand_local_paths`/`respond_offer.save_dir` 无门禁:WebView 一旦失守(XSS/依赖投毒)→ 枚举任意目录/任意落盘/执行文件(tauri.conf.json:11 + capabilities/default.json)
- ✅ 已修(3fcaaad+a67a1cf) M-A4 relay_psk 明文回传 WebView(get_settings)+ config.json 明文落盘 exe 同目录,无 ACL;identity.key 同落公共可读位置(commands.rs:1633 + store.rs:58)
- ✅ 已修(0326683+fc92a66) M-A5 秒传 skip 表 = hash oracle:已配对对端即使被 Deny 也能探测本机收件箱是否持有某 hash 对应文件(engine.rs:1969-1986)
- ✅ 已修(3fcaaad) M-A6 中继 info 日志持久记录指纹↔公网 IP↔NAT 漂移轨迹(data_plane.rs:180)
- ✅ 已修(3fcaaad) M-A7 复制 IP 后剪贴板永不清理(Android 13 剪贴板历史/输入法可长期读,泄露内网拓扑)
- ✅ 已修(3fcaaad+a67a1cf) M-A8 原始 Rust 错误直出 UI/console 含本机绝对路径(api.ts:47 等)

**会话与状态机**
- ✅ 已修(1c726dd+f1ed95c) M-B1 同指纹会话覆写+旧 ctrl_loop 无条件 remove+门超时按 fp 寻址:重连竞态把健康会话踢出表,传输报"会话不存在"(session.rs:1128,1137-1155)
- ✅ 已修(1c726dd) M-B2 load_config 读失败(非解析失败)也重置并**回写**默认配置:AV 瞬时锁文件 = 共享区/中继设置全丢且不可逆(store.rs:66-77)
- ✅ 已修(1c726dd+f1ed95c) M-B3 relay 重连竞态:旧 connect 任务不 abort,完成后旧地址连接覆盖新配置(relay_state.rs:88 + commands.rs:1979)
- ✅ 已修(07ba31a) M-B4 前端 15s 超时后 Rust 侧仍占响应通道至 20s,重试全撞"通道被占用"(commands.rs:250-354)
- ✅ 已修(b9dee83) M-B5 FFI list_remote/share_op 三兄弟独占响应通道 10s,与传输互斥(ffi lib.rs:1716-1873)
- ✅ 已修(80b6ca6+2d7ce00) M-B6 FFI `state.lock().unwrap()` 全覆盖:一次持锁 panic → 毒锁 → 全接口连锁瘫痪
- ✅ 已修(ce69eb6) M-B7 push 进度泵 `transfer_get_mut().await.unwrap()`:并发删行即 panic 杀死事件泵(ffi lib.rs:2494)
- ✅ 已修(ce69eb6) M-B8 list_dir 全量物化+排序后才分页:百万条目录 ~100MB 峰值(share.rs:143-206)

**资源与性能**
- ✅ 已修(ce69eb6) M-C1 legacy jobs 表持有全量 Manifest 只进不出(2.2MB/任务,engine.rs:1593,1747)
- ✅ 已修(ce69eb6) M-C2 .localtrans-parts 任务目录零 GC:放弃续传的任务永久占磁盘,可达整文件大小
- ✅ 已修(ce69eb6) M-C3 source_probe 空闲计数 `0.5 as u64==0` 截断:60s 自杀失效,探针永久 500ms 空转(source_probe.rs:59-62)
- ✅ 已修(ce69eb6) M-C4 进度通道背压进数据路径:UI 卡 >90s 会让活跃传输被看门狗误杀(engine.rs:1484-1490)
- ✅ 已修(ce69eb6) M-C5 三处跨 await 持 sender_jobs 写锁等 progress 通道:慢消费者放大为全局停摆
- ✅ 已修(ce69eb6) M-C6 每 FetchReq 深拷贝整份 Manifest(100GB 文件≈56GB 累计 memcpy)
- ✅ 已修(ed1b2d1) M-C7 同步重 IO 在 async 上下文:finalize sync_all、fs::read、expand_local_paths BFS、netsh 子进程、get_network_status 无缓存
- ✅ 已修(1c726dd) M-C8 setFilter 组合期调用:每重组重查全量 MediaStore(FilesScreen.kt:181)
- ✅ 已修(1c726dd) M-C9 绕过 Repo 的主线程 FFI 直调三处(validateRelay/respondDelete/respondOffer)

## 六、Low 清单(25 条,摘录)

全部 ✅ 已修:b9add4f(core/relay 12 项)+87b6862+049f561+bfd2407(双壳 13 项)。

 cooldown 表过期不清理;小通道 send 无超时;≥250GB 文件 MetaResp 超 4MiB 上限断会话;share_watch 同步扫目录;small_batch_streams 只增不减;task_controls 错误路径残留;InboxIndex 每文件全量重写+固定 tmp 名并发竞态;PSK 摘要仅 32-bit 可字典验证;Register.name 无长度上限广播放大;PSK 比较非常时(短路);hidden 设备 Punch 在线 oracle;无带宽/准入控制(PSK 泄露=开放隧道);发送完成也弹"接收完成"通知;搜索过滤破坏性收缩;传输历史 saved_files 不清理;push 方向 migrate 误丢(桌面);set_inbox_dir 竞态;PairingDialog 监听器注销竞态;主线程 stat IO;死 ticker;接收文件公共 Download 可被他 App 读(产品取舍,建议文档明示)。

## 七、已验证安全的正面结论

- 配对码:thread_rng 无偏生成、ct_eq 恒时比较、码绑连接态不可重放、3 次+300s 冷却熵充足
- 无降级向量:两侧强制 TLS1.3、ALPN 固定、DATA_VERSION 校验
- 日志无 PSK/配对码明文(中继侧仅 sha256 前 8 hex 摘要)
- 推送侧落盘净化完整(sanitize_file_name/rel_dir 逐段/ParentDir 全拒)
- Vue 全量 grep 无 v-html/innerHTML;localStorage 零敏感存储;Manifest 无 cleartext;FileProvider 仅 Download 子树
- firewall.rs 无参数注入;devtools release 关闭
- 锁顺序全库一致无 inversion;take_msg 取消安全

## 八、误报剔除(亲验记录)

- FFI 代理"范围外备注"称 push Started `progress.send` 缺 `.await` → **核实为误报**(engine.rs:985-1016 全部带 .await,含对照点 597/1244)
- 其余 5 条亲验(C1/C2/S1/T1/T3)代码证据全部属实

## 九、修复路线建议

**P0(当天可修,改动小收益大)**:C1/C2;S5(一行上限);S4(锁内快照+push 超时);T3(两处补 return);S9(拉取侧 parent 净化);M-B2(读失败不回写);M-A1(allowBackup=false 一行);M-A2(logcat 脱敏)。
**P1(本周期)**:S1 指纹钉扎(安全价值最高);S2/S3(注册签名+成员校验);S6 端口 TTL;S7 认证超时+连接上限;T1/T2(BufferPool RAII 化);T6(connect 超时+锁外 block_on);T7(IO dispatcher);M-C3(一行修 CPU)。
**P2(排期)**:T4/T5 路由器架构改造(spawn 化);M-B6 FFI 锁重构;M-A3 CSP/opener 收敛;M-A5 秒传 oracle;其余 Medium/Low 随版本消化。
