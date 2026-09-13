# P4 执行计划：性能基线与门禁

> Spec: specs/2026-09-07-p4-polish-performance.md
> 环境注意：huss_laptop 不可达——吞吐基线用 PC↔手机（WiFi 实测口径标注）或环回；中继吞吐留双机遗留。

## 任务分解

### T1 直连吞吐基线（PC→手机 WiFi 主口径）
- [x] 场景 perf-baseline.mjs：1GB 单文件推手机 ×3 取中位（WiFi 实测口径）+500MB PC-PC LAN 环回对照（可达才跑,条件段）；采样 MB/s/CPU/内存；3 次取中位
- [x] 报告落 docs/audit/perf-baseline.md（lib/perfreport.mjs 自动再生,含 T3/T4 段）
### T2 小文件批量
- [x] 200×10KB push_files 单 offer（perf-baseline.mjs 第二段）——总耗时+速率记录,父卡 done 断言
### T3 前端渲染
- [x] 1000 done 卡（transfers.json 预置+单机重启,无 debug seed 端点的最小方案）展开首屏构造时长断言 <2s+交互 DOM 节点计数（perf-render.mjs）；滚动流畅度=人工目检项标注
### T4 空闲足迹
- [x] 无传输 5 分钟：每 30s 采样进程 CPU（CIM,单核口径）,均值 <2% 断言；>5% 单点记周期扫描疑点（perf-idle.mjs）
### T5 性能门禁
- [x] 三 perf 场景入 run-all（标签「性能基线:*」,quick=false 发版必跑,perf-baseline timeoutMin=45 慢链路放宽;设备不可达场景内条件跳过 exit 0）
- [x] 回归判定说明（吞吐 ±20% 标红口径）写入 docs/audit/perf-baseline.md 尾注
### 收尾
- [ ] 门禁三泳道；RELEASE P4 状态；BACKLOG 收尾

## 执行记录

- 2026-09-08（凌晨真机窗口）：huss_laptop 起初不可达部署走 `--skip-huss_laptop`（实测 HTTP/SSH 中途恢复,环回对照段因此跑成）。三场景全绿：
  - perf-baseline 8/8：T1 1GB×3 中位 **2.6 MB/s**（2.6/2.6/2.6,387-391s,字节 1GiB 核对过）,发送端 CPU 中位 2.4%、结束内存 ~64MB；T2 200×10KB 单 offer 19.93s 父卡 done。观测：发送端 speedBps 显示口径中位 ~64MB/s 与秒表端到端 2.6MB/s 差 ~25 倍（done 按 ~8MB QUIC 突发推进,EMA 偏乐观）——留打磨池核查。
  - perf-render 5/5：历史 (1000),1000 卡展开首屏 **110ms**（<2s 断言）,交互 DOM 节点 2007。
  - perf-idle 3/3：空闲 CPU 均值 **0.00%**（峰值 0.0%,无 >5% 单点=未见周期扫描异常）,内存 38→34MB。
- 环境备注：测试时段开发机屏幕锁定,窗口截图拍到锁屏壁纸——渲染正确性以 DOM 断言为准（shot.rs「截图仅作证据」原则）,已在基线报告标注。
- 新增：scenarios/perf-{baseline,render,idle}.mjs、lib/winmetrics.mjs（CIM 采样,免 locale 依赖）、lib/perfreport.mjs（基线报告再生）；run-all.mjs 注册+per-scene timeoutMin。产品代码零改动。

## P4 关门 ✅（2026-09-09）
- cd75aad：perf-baseline/perf-render/perf-idle 三场景+winmetrics/perfreport 工具+run-all 注册（per-scene timeoutMin）
- 基线（PC→手机 WiFi 真机）：1GB×3 中位 2.6MB/s（CPU 2.4%/内存 64MB）；200×10KB 批量 19.9s；1000 卡首屏 110ms；空闲 5min CPU 0.00%
- **发现**：发送端 speedBps 显示口径虚高 ~25 倍（QUIC 突发+EMA 乐观）——打磨池挂"显示速度口径核查"（P1 的 EMA 是显示层，此处指 store 数据源口径，需区分）
- 门禁：shell 83 绿；产品代码零改动
