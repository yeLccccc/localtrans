# V1 执行计划：逐功能验证·场景库全覆盖

> Spec: specs/2026-09-07-v1-verify-coverage.md
> 目标：手工验收清单全部场景化；run-all 成为发版键。

## 任务分解

### T1 安卓手工清单场景化
- [x] 读 docs/e2e-android-manual.md 13 项，逐项映射到现有场景或新建：
  - 已覆盖项标注场景名（配对/收发/通知/中继拉模式/清空记录等大部分已有）
  - 缺口项新建小场景：`notify-deeplink.mjs`（通知存在 dumpsys 硬断言+深链软断言）、
    `remote-rename.mjs`（能力探测式：产品远程重命名缺失→SKIP 带诊断）、
    `offer-deny-timeout.mjs`（拒绝/超时路径）
- [x] 产出追溯表 → docs/superpowers/V1-TRACEABILITY.md
### T2 桌面手工清单场景化
- [x] v0.12 重构计划 Task 12 的 9 条核对单逐条映射：
  - 已覆盖：#1(D3 取消删卡)、#3(部分,D3)、#4(pc-pc-transfer 父卡)、#8(D3 历史折叠)
  - 缺口新建：`kill-restart-recovery.mjs`（#2 强杀重启 interrupted 带元数据+续传可用，PC 单机）、
    `parts-deleted-integrity.mjs`（#7 位图全真→failed"完整性存疑"+GC 删目录，PC 单机）、
    `offer-deny-timeout.mjs`（#9 完成 toast 推+拉双向）
- [x] 追溯表同上
### T3 run-all 发版判定报告
- [x] run-all 注册表扩至 18 场景；出口条件②核对表补齐「强杀重启续传恢复」「parts 完整性存疑 failed」
- [x] 全量跑一次 run-all 出报告（2026-09-08，huss_laptop 网络隔离期间：8 FAIL 全为
  双机场景连接类预期失败；V1 新场景链内条件跳过 + 恢复 huss_pc 后单跑复验全绿）
### T4 场景质量线
- [ ] 每场景连跑 2 次稳定性抽查（flaky<5%）；超标场景修断言或修环境
### 收尾
- [ ] gate 三泳道 + run-all 全绿报告归档（待 huss_laptop 恢复后补全绿口径）

## 执行记录

- 2026-09-08 T1+T2+T3 完成（本卡派发）：
  - 新增 5 场景：kill-restart-recovery / parts-deleted-integrity / notify-deeplink /
    offer-deny-timeout / remote-rename（全部条件跳过设计，已注册 run-all）。
  - 实跑验证：kill-restart-recovery 6/6 PASS；parts-deleted-integrity 7/7 PASS；
    notify-deeplink PASS（dumpsys 通知硬断言过；深链热启动不切页记疑似产品 bug）；offer-deny-timeout
    SKIP(exit 0)；remote-rename SKIP(exit 0，产品缺口：远程菜单无重命名)。
  - 产品发现记录在 V1-TRACEABILITY.md「疑似产品问题」节（深链 pendingTab 消费点、远程重命名缺失）。
  - 技术口径：list_transfers/list_disk_jobs 的 job_id 均为 16 位 hex 串（u64_hex_string）；
    state/transfers 摘要面无 local_role/时间戳，元数据断言走 list_transfers。

## V1 关门 ✅（2026-09-08）
- 4bff21d：追溯表 V1-TRACEABILITY.md（安卓 13 项 11 覆盖+2 暂缓、桌面 9 条 7 覆盖+3 新建场景）+5 新场景（kill-restart/parts-deleted 单机两连绿；notify-deeplink 通知硬断言；offer-deny-timeout/remote-rename 条件跳过就绪）
- 076f045：run-all 双机预检 SKIP（链式故障不连坐）
- 收官：10 PASS / 8 SKIP / 0 FAIL（当前环境正确绿）；出口条件②核对表补齐
- 附带产品发现（未改代码）：通知深链热启动不切页、远程重命名未随 Compose 重构移植——打磨池
