# 契约 · data-testid 命名表（Task M2，spec §7.2/§13）

| 项 | 值 |
|---|---|
| 日期 | 2026-09-06 |
| 状态 | M2 首版（4 页 + 7 组件关键控件已铺，遗留项见文末） |
| 上游 | `docs/specs/0001-e2e-test-automation.md` §7.1.4（tree 契约）、§7.2（铺设要求） |
| 使用方 | testBridge 选择器（`[testid=x]` 简写）、`/api/ui/tree` 的 `testId` 字段、Android `testTagsAsResourceId`（M7 共用本表） |

## 1. 命名规范

1. **kebab-case**，全小写，单词连字符分隔。
2. 结构 **`区域-元素[-动作/修饰]`**：
   - `区域` = 页面名（devices/transfers/browse/settings）或组件域（nav/device/transfer/pairing/push-wizard/confirm）；
   - `元素` = 控件语义（language-select、device-name-input、share-add-btn）；
   - 后缀表明控件类型：`-btn`（button）、`-input`（text/number/password 输入）、`-select`（下拉）、`-toggle`（checkbox 开关）、`-link`（router-link/a）、`-chip`（状态胶囊按钮）。
3. **动态列表项用稳定前缀 + 业务 id**：`transfer-item-{job_id}`、`device-card-{fingerprint}`、`browse-share-item-{share.id}` 等。选择器用 **`^=` 前缀匹配**：`[testid^=transfer-item-]`（配合 testBridge 简写语法，见 test-api.md §4）。
   - 列表行内部的**动作按钮是静态 id**（如 `transfer-cancel-btn`）：同一时刻每行只渲染一个状态分支，一行内不重复；跨行匹配时先用前缀选中行再后代组合：`[testid^=transfer-item-] [testid=transfer-cancel-btn]`。
4. **只加属性，不改结构/逻辑/样式**（M2 铺设纪律）；历史遗留的旧风格 id（`btn-grant`、`code-input` 等）不重命名——已有测试依赖，保持冻结，新元素一律走本规范。
5. 命名冲突避免：同页唯一控件不加业务 id；同语义多实例必须挂动态前缀。

## 2. 已铺清单

### App.vue（全局骨架）

| testid | 元素 |
|---|---|
| `nav-devices-link` / `nav-transfers-link` / `nav-browse-link` / `nav-settings-link` | 底部导航 4 个 router-link（动态绑定 `nav-${tab.icon}-link`） |
| `offer-countdown` *（旧）* | 推送确认倒计时 |
| `delete-countdown` *（旧）* | 远程删除确认倒计时 |
| `btn-expand-files` *（旧）* | 推送弹窗文件列表展开 |

### pages/Devices.vue

| testid | 元素 |
|---|---|
| `devices-local-ip-chip` | 本机 IP 复制胶囊 |
| `devices-hidden-toggle` | 隐身开关 |
| `devices-manual-add-open-btn` | "+" 打开手动添加对话框 |
| `devices-refresh-btn` | 刷新（探测网络） |
| `devices-error-retry-btn` | 错误态重试 |
| `devices-manual-addr-input` | 手动添加地址输入 |
| `devices-manual-cancel-btn` / `devices-manual-submit-btn` | 手动添加对话框 取消/添加 |
| `btn-push-wizard` *（旧）* | 推送文件按钮 |

### pages/Transfers.vue

| testid | 元素 |
|---|---|
| `transfers-disk-history-toggle-btn` | 历史记录（磁盘）展开 |
| `transfers-clear-completed-btn` | 清除已完成/失败 |
| `transfers-error-retry-btn` | 错误态重试 |
| `transfers-history-fold-btn` | 历史（视图内）折叠开关 |
| `transfers-disk-item-{job_id}` | 磁盘历史行（动态前缀） |
| `transfers-disk-restore-btn` / `transfers-disk-destroy-btn` | 磁盘历史 恢复到列表/彻底删除 |

### pages/Browse.vue

| testid | 元素 |
|---|---|
| `browse-device-chip-{fingerprint}` | 设备选择标签（动态前缀） |
| `browse-goto-devices-link` | "去连接" 跳设备页 |
| `browse-shares-refresh-btn` | 共享区刷新 |
| `browse-share-item-{share.id}` | 共享区列表项（动态前缀） |
| `browse-go-up-btn` | 返回上一级 |
| `browse-files-refresh-btn` | 文件列表刷新 |
| `browse-file-check-{file.name}` | 文件勾选框（动态前缀，名称可能含特殊字符时建议改用行内索引——见遗留） |
| `browse-dir-download-btn-{file.name}` | 文件夹整下按钮（动态前缀，同上） |
| `browse-download-selected-btn` | 下载选中 |

### pages/Settings.vue

| testid | 元素 |
|---|---|
| `settings-share-add-btn` | 共享区添加 |
| `settings-share-item-{share.id}` | 共享区行（动态前缀） |
| `settings-share-remove-btn` | 共享区删除（行内静态） |
| `settings-download-dir-change-btn` | 下载目录修改 |
| `settings-device-name-input` | 本机设备名 |
| `settings-trusted-item-{fingerprint}` | 信任设备行（动态前缀） |
| `settings-peer-alias-input` | 信任设备备注输入（行内静态） |
| `settings-trusted-remove-btn` | 移除信任（行内静态） |
| `settings-firewall-add-btn` | 添加防火墙规则 |
| `settings-firewall-logs-btn` | 打开日志 |
| `settings-relay-enabled-toggle` | 中继开关 |
| `settings-relay-server-input` / `settings-relay-psk-input` | 中继服务器/密钥 |
| `settings-relay-save-btn` | 保存并连接 |
| `settings-consent-timeout-input` | 同意超时 |
| `settings-offer-timeout-input` | 推送确认超时 |
| `settings-max-active-input` | 并发任务数 |

### components/TransferItem.vue

| testid | 元素 |
|---|---|
| `transfer-item-{job_id}` | 卡片根（动态前缀，**文档级锚点**） |
| `transfer-expand-btn` | 子项展开 |
| `transfer-pause-btn` / `transfer-cancel-btn` | 暂停/取消（active、source-push 同语义复用） |
| `transfer-resume-btn` | 继续（paused） |
| `transfer-retry-btn` | 重试（destination failed/interrupted） |
| `transfer-resume-pending-btn` | 续传 |
| `transfer-remove-btn` | 删除（终态通用） |
| `transfer-open-folder-btn` | 打开所在文件夹 |
| `transfer-throttle-btn` / `transfer-kick-btn` | 限速/踢人（source-pull） |
| `btn-resend` *（旧）* | 重发（source-push failed） |

### components/DeviceCard.vue

| testid | 元素 |
|---|---|
| `device-card-{fingerprint}` | 卡片根（动态前缀） |
| `device-connect-btn` | 连接（未信任） |
| `device-browse-btn` | 浏览（已信任） |
| `device-menu-btn` | "⋮" 下拉菜单 |
| `device-perm-browse-toggle` / `device-perm-download-toggle` | 权限开关 |
| `device-perm-push-select` | 推送策略下拉 |
| `device-remove-trust-btn` | 移除信任 |
| `btn-push-menu` *（旧）* | 菜单内"推送文件…" |

### components/PairingDialog.vue

| testid | 元素 |
|---|---|
| `pairing-dialog` | 对话框根 |
| `pairing-close-btn` | 右上 "×" |
| `pairing-cancel-wait-btn` | A 侧等待阶段 取消 |
| `pairing-cancel-entry-btn` | A 侧输入码阶段 取消 |
| `btn-deny` / `btn-grant` *（旧）* | B 侧同意门 拒绝/同意 |
| `btn-cancel-wait` *（旧）* | B 侧亮码阶段 结束等待 |
| `code-input` / `btn-submit` *（旧）* | A 侧配对码输入/提交 |

### components/PushWizard.vue

| testid | 元素 |
|---|---|
| `push-wizard-device-{fingerprint}` | 步 1 设备单选行（动态前缀） |
| `push-wizard-next-btn` / `push-wizard-back-btn` | 下一步/上一步 |
| `push-wizard` / `btn-pick-files` / `btn-pick-folder` / `btn-send` *（旧）* | 根/选文件/选文件夹/发送 |

### components/ConfirmDialog.vue（全旧，够用）

`confirm-dialog` / `confirm-input` / `confirm-cancel` / `confirm-ok`

### components/CodeBadge.vue / NavIcons.vue

纯展示，无可交互元素，未铺（NavIcons 图标本身 `aria-hidden`，会从 tree 剪枝）。

## 3. 待铺/遗留（宁缺勿滥，本轮不动）

- `PushWizard` 两个"取消"按钮（步 1/步 2 各一，静态 id 会重复；待场景真正需要时改 `push-wizard-cancel-step1/step2-btn` 或合并入口后定名）。
- `Browse` 文件行主体（目录进入点击区）未铺：目标是大面积 div，`^=` 前缀已覆盖勾选/下载控件，进入目录可由双控件间接驱动。
- `Transfers` 子项行（children-panel 内 child-row）的"重试"未铺：M3 快照断言落地后再按需补 `transfer-child-retry-btn`。
- 文件名含空格/引号时 `browse-file-check-{name}` 的 CSS 转义问题：M2 冒烟不触碰，M5 场景化后若成为障碍改为行索引 `browse-file-row-{index}`。
- 旧风格 id 的收敛：不计划迁移（Android 侧将共享同名约束），仅在新增元素时执行新规范。

## 4. Android 侧已铺清单（M7，Jetpack Compose `Modifier.testTag`）

规则与 §1 一致；根布局（AppNav Scaffold）挂 `Modifier.semantics { testTagsAsResourceId() }`，
testTag 以 `resource-id` 形式出现在 `uiautomator dump`（编排器 `lib/adb.mjs` 的 `testTag` 选择器即读它）。
与桌面同名 = 跨端场景同一语义 id 复用；桌面未覆盖的 Android 特有元素按新规范命名。

### AppNav（全局骨架）

| testTag | 元素 |
|---|---|
| `nav-devices-link` / `nav-files-link` / `nav-transfers-link` / `nav-settings-link` | 底部导航 4 项（与桌面 nav-*-link 同表） |

### 设备页（DevicesScreen）

| testTag | 元素 |
|---|---|
| `devices-hidden-toggle` | 隐身模式开关（与桌面同名） |
| `devices-local-ip-chip` | 复制本机 IP 按钮（桌面同语义为胶囊） |
| `devices-manual-add-open-btn` | "+" 手动添加入口（与桌面同名） |
| `device-card-{fingerprint}` | 设备卡片根（动态前缀，与桌面同构；卡片整体即"连接"点击区） |
| `devices-manual-addr-input` / `devices-manual-submit-btn` / `devices-manual-cancel-btn` | 手动添加对话框 输入/探测/关闭（与桌面同名） |

### 传输页（TransfersScreen）

| testTag | 元素 |
|---|---|
| `transfers-clear-completed-btn` | 清空记录（与桌面同名） |
| `transfer-item-{jobId}` | 传输卡片根（动态前缀，与桌面 `transfer-item-{job_id}` 同构） |

### 设置页（SettingsScreen）

| testTag | 元素 |
|---|---|
| `settings-device-name-input` / `settings-relay-enabled-toggle` / `settings-relay-server-input` / `settings-relay-psk-input` | 与桌面同名 |
| `settings-save-btn` | 整页保存按钮（桌面为分卡保存，Android 特有名） |
| `settings-test-auto-consent-toggle` | **debug 构建专属**：配对自动同意测试钩子开关（release 编译期剔除，桌面无对应物） |

### 文件页（FilesScreen）

| testTag | 元素 |
|---|---|
| `files-location-local-tab` / `files-location-remote-tab` | 本机/远程位置切换（远程≈桌面 browse 域入口） |
| `files-create-folder-fab` | 新建文件夹 FAB |

### 配对对话框（PairingDialogs）

| testTag | 元素 |
|---|---|
| `btn-grant` / `btn-deny` | B 侧同意门 同意/拒绝（沿用桌面共表冻结旧名，跨端场景直接复用） |
| `code-input` / `btn-submit` | A 侧配对码输入/提交（同上，冻结旧名） |
