# 并行任务板

工作流：任务卡状态 `待认领 → 进行中(注明 agent/分支) → 待评审 → 已合并`。
认领方式：把本行状态改为 `进行中` 并写上分支名（`agent/<任务名>`），同时 `bash scripts/wt.sh new <任务名> <泳道>`。
规则：同泳道串行、跨泳道并行；跨层改动先走契约卡；完成定义见 `AGENTS.md`。

## 状态：P0 基建（解锁并行的前提）

| 卡 | 泳道 | 任务 | 验收标准 | 状态 |
|---|---|---|---|---|
| P0-1 | core | **测试端口可配置化**：发现/QUIC/中继监听端口支持 `LOCALTRANS_TEST_PORT_BASE` 环境变量偏移 | 设 `LOCALTRANS_TEST_PORT_BASE=50100` 后 `gate.sh core` 全绿且不占 47600/47601；不设变量时行为与现状完全一致 | 待认领 |
| P0-2 | infra | **仓库卫生**：根目录 `test_output.txt`、`control_test_final.txt`、`relay_all_tests.txt`、`relay_test_output.txt`、`test_serde_format.rs`、`tmp/` 归档（移入 `docs/audit/archive/` 或删除）；根目录 `test_serde_format.rs` 若有价值并入 core 测试 | `git status` 干净；`gate.sh core` 全绿 | 待认领 |
| P0-6 | shell | **已解决(2026-09-07)**：真实配对"死锁"根因 = PairingDialog 仅在 Devices.vue 页内挂载，配对事件监听器随组件存活——前端离开设备页时同意门永不出现(所有自动化失败场景 UI 均停在传输/设置页;23:47 手点成功恰在设备页)。修复:弹窗提升至 App.vue 全局挂载。真机验证:PC 停传输页+手机发起+全自动(同意→读码→输码→提交)配对闭环,码匹配+双向信任写入+会话建立。**注:get_settings 的 trustedPeers 有前端缓存滞后(配对完成不刷新),次要问题另记** | 同意门任意页面出现 ✓ 已达成 | 已完成 |
| P0-7 | ffi | **已定案(2026-09-07,重新分类)**：原判"会话入站秒死"经三点点位探针(ctrl_fwd/router_got/ffi_ask)证伪——**OfferReq 全链通畅,OfferSheet 正常弹出,2MB 推送真机完成落盘**。此前"无弹窗"为编排器两处工具缺陷叠加假象:①logcat 按桥 tag 过滤漏行+时窗错位(误判 Kotlin 未收到事件);②接收按钮的 clickable 节点与文本节点分离,按钮匹配器恒 miss→60s 自动拒绝→推送超时→连接闲置死(31s,次生现象)。真实验收后残余:手机长时间运行后 post_handshake 挂起(重启恢复,P2 另查);PC config 残留旧共享路径(work/share 非 exe 旁默认,夹具问题已绕开) | 推送/拉取跨端可用(推送段✓,批量拉取 UI 自动化待下轮) | 已完成(主体) |

> P0-1 落地前：relay 泳道全局同时只能跑一个；落地后按 worktree 端口段并行。

## 只读任务（零冲突，可大量并行）

| 卡 | 泳道 | 任务 | 产出物 | 状态 |
|---|---|---|---|---|
| R-1 | read | **测试覆盖审计**：盘点 `localtrans-core`（重点 `transfer/`、`serde_compat.rs`、`identity.rs`）与 `localtrans-relay` 的未覆盖分支，按"高价值×易写"排序 | 报告卡：建议补充的测试清单（模块/函数/场景） | 待认领 |
| R-2 | read | **安全审计**：`pairing.rs` 配对码流程、`identity.rs` 密钥体系、HKDF/subtle 用法、relay 控制面鉴权、续传 manifest 校验 | 报告卡：风险点按严重度排序，附 file:line | 待认领 |
| R-3 | read | **跨平台行为差异审计**：Windows 壳（src-tauri）与 Android 壳（ffi）对同一 core 接口的处理差异（防火墙、路径、权限、事件转发） | 报告卡：不一致清单 + 建议统一方案 | 待认领 |

## 写任务示例（按泳道认领）

| 卡 | 泳道 | 任务 | 备注 |
|---|---|---|---|
| W-1 | infra | 契约先行流程示范：起草《协议/DTO 变更操作手册》（先合定义→各壳并行适配→集成顺序） | 纯文档，为后续跨层改动立规矩 |
| W-2 | relay | （占位）R-2 安全审计产出后，取 Top1 风险点修复 | 依赖 R-2 完成 |
| W-3 | read | 按 R-1 清单补充高价值单元测试（只加 `#[cfg(test)]`，不改产品代码） | 与 R-1 不同 agent，形成流水线 |

## 集成者职责（人 or orchestrator 会话）

1. 串行处理"待评审"分支：`git fetch hub`（备份）→ review diff → `bash scripts/gate.sh all` → merge → `git push hub main`。
2. 合并后立即在任务板更新状态，删除对应 worktree（`wt.sh rm`）。
3. 发现跨层耦合时，把任务拆成"契约卡 + 实现卡"，不要让单分支跨泳道。
