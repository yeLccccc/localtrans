# M1 执行计划：稳定性与测试地基

> Spec: specs/2026-09-07-m1-stability-foundation.md
> 纪律：逐任务 TDD/先行验证→gate→原子提交；跨 PC/手机真机验证由主会话执行（subagent 无设备权限）。

## 任务分解

### T1 core: 测试端口可配置化（LOCALTRANS_TEST_PORT_BASE）（待评审）
- [x] 发现端口/QUIC 端口读 env 偏移（默认 47600/47601，不设=行为逐字节一致）
- [x] 全仓 grep 清零测试代码硬编码 47600/47601（`tests/`、`crates/*/tests`、`#[cfg(test)]`）
- [x] 单测：设 env 后端口=base+偏移；不设=默认
- [x] 验证：`LOCALTRANS_TEST_PORT_BASE=50100 bash scripts/gate.sh core` 全绿且 netstat 无 476xx
- 提交：`feat(core): 测试端口LOCALTRANS_TEST_PORT_BASE可配置化`
- 备注：①实现为 `crates/localtrans-core/src/ports.rs` 纯函数 `resolve(Option<&str>)`（不缓存 env，可测）+ `discovery_port()/quic_port()` 包装；消费点=store::Config 默认值、discovery::DiscoveryConfig 默认值(含广播 target)、ffi target/parse_probe_addr、examples 两处。②src-tauri firewall 规则/提示文案仍为字面量 47600-47601（壳层生产代码，T1 范围外；若 e2e 将来带 env 启动 app，壳层防火墙跟随需另开任务）。③`push_same_large_file_twice_second_is_instant` 偶发时序闪失（与端口无关，单跑 3/3 绿，全量重跑绿），疑似存量 flaky。

### T2: L3 出厂重置
- [x] `reset(3)`：停进程→data/ 清空（config/identity/trust/transfers/probe_targets）→可选信任播种→重启→health
- [x] 手机侧 `pm clear` 封装 + MIUI 权限弹窗自动放行（固化本周实证流程）
- [x] e2e 场景 `l3-reset.mjs`：L3 后断言双端如新装（无信任/默认共享区/新身份）
- 提交：`feat(测试): L3出厂重置(reset(3)+手机factoryReset+种子信任+场景)`
- 备注：①`seedTrust` 支持 数组(同表)/对象(按目标名分表)，配 `keepIdentity`(保指纹) 才能让播种条目指向有效身份——场景用它做播种演练+真实 connect 建会话。②夹具自愈=停双端→全量回灌备份(data 六文件，含身份/config/「我的手机」信任)→L2 重启；失败路径也兜底回灌。③transfers 断言放行 interrupted 断点续传卡（downloads/.localtrans-parts 磁盘残留重建，非 data 清理失败）。④trustedPeers 断言走 `list_trusted`（get_settings=ConfigDto 无此字段）。⑤双端真机 9/9 PASS + 夹具指纹/connect 复验通过；huss_laptop 截图 500(显示器离位)为存量环境问题。

### T3: 冷启动配对场景（常态化 P0-6 教训）
- [ ] `cold-start.mjs`：双端 L3→全新身份→真实配对（同意门+读码+输码）→推送→拉取→删信任重来
- [ ] offer-modal 补 testid（App.vue）+ 场景改用 testid
- 提交：`feat(ui): offer弹窗testid` + `feat(测试): 冷启动配对场景`

### T4: run-all 入口
- [ ] `run-all.mjs`：串跑全部场景（可配置顺序/失败继续/汇总报告含出口条件②核对表）
- [ ] 接入现有 5 场景 + T2/T3 新场景
- 提交：`feat(测试): run-all一键全场景+出口条件核对报告`

### T5: 中继 e2e 场景（待评审）
- [x] WSL/本机起 relay（独立端口段）；fw-once 变体制造"直连不可达"
- [x] `relay-path.mjs`：隔离→名片式手动添加(IP)→配对→经中继传输→字节核对→恢复防火墙
- 提交：`feat(测试): 中继路径场景(relay封装+隔离模拟+经中继传输)`
- 备注：①relay 实际落位 huss_laptop（其活动 profile 防火墙关闭、入站免规则；huss_pc 三 profile 全启用且 agent 会话无管理员权限加规则，本机起 relay 则 huss_laptop 的控制/数据面入站必被拦——详见 `tests/e2e/lib/relay.mjs` 头注）。②"直连不可达"降级为行为级隔离：双端监听/广播端口错开（杀进程→补丁 data/config.json 端口→重启；壳层绑定端口读持久化 config，LOCALTRANS_TEST_PORT_BASE 默认值仅首启生效）；netsh 阻 47600-47601 需管理员权限，不可行。③断言等级="配置中继下名册可见（viaRelay=true+relay 租约地址）→connect 建会话→5MB 真实传输（非秒传）"，relay debug 日志 Punch/会话端口分配/KNOCK 为进程级路径证据。④发送端 done×2（e2e-harness §8 已知bug）按整数倍容差记档不失败，接收端字节严格核对为交付权威。⑤huss_laptop 截图 500（显示器离位，同 T2 备注⑤存量环境问题）降级记档不判 FAIL。⑥场景指纹全运行期采集——执行期间双端恰被 T2/T3 验证 L3+冷启动重置重配（指纹全换），场景自动适配，验证了不钉死指纹的设计。

### 收尾
- [ ] run-all 全绿报告归档；BACKLOG/RELEASE 状态更新；门禁三泳道绿

## 执行记录
- T1 完成（2026-09-07）：ports 模块落地，全仓测试硬编码清零，gate core env=50100/默认双绿，src-tauri 67 绿，ffi 40 绿（env=50100 串行）。待评审。
- T5 完成（2026-09-07）：relay-path 10/10 PASS×2 连跑（runId relay-path-2026-09-07-05-01-59-ndp1 / 05-02-58-jkvp）。拓扑=relay@huss_laptop(19443/19500-19520)+双端端口基址错开隔离；经中继会话与 5MB 传输全链断言绿，清理（UI 停用 relay/恢复 config.json 端口/L2 回默认/L1/停 relay 删任务）实测环境零残留。待评审。
- **M1 关门 ✅（2026-09-08）**：92cdeae（T0 修复归位后 run-all **7/7 全绿** exit=0，报告 reports/run-all-m2t0/）。注：T0（跨端弹窗）修复实测根因在 e2e 断言层假阴性+OfferSheet testid 缺失（subagent 92cdeae），原"Kotlin 事件流丢事件"假设被推翻——探针证明事件链无丢失。M1 状态：**已完成**。
