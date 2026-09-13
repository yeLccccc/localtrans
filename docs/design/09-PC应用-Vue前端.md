# 09 PC 应用 — Vue 前端

> 源码:`ui/src/`(App.vue / pages×4 / components×6 / stores×4 / api.ts / types.ts / composables / directives / styles/design.css)。

## 1. 技术选型

Vue 3(`<script setup>`)+ Pinia(setup 风格)+ Vue Router(4 页懒加载)+ Vite + Vitest + @vue/test-utils;Tauri IPC 直接 `@tauri-apps/api/core invoke`(**无 adapter 层、无 __TAURI__ 检测** —— 纯浏览器降级仅 test-support/tauriMock.ts)。

## 2. 路由与壳(App.vue)

- 路由:`/devices`(默认)/ `/transfers` / `/browse` / `/settings`,无守卫。
- App.vue:底部 4 tab 导航(NavIcons SVG)+ 右上 toast 容器 + 四个全局模态:
  - **推送确认弹窗**(offer-request):接收 / 另存到…(先 offerExtend 顺延 + 倒计时 reset)/ 拒绝;超时自动关+"已超时自动拒绝";应答失败也强制关弹窗给用户出路;
  - **远程删除确认**(delete-request):倒计时 + 允许/拒绝;
  - **关闭拦截**(useCloseGuard):活动任务时二次确认;
  - **启动断点恢复提示**:interrupted 且有 parts → "全部恢复"逐个 resumePending / 忽略。

## 3. 页面设计

### 3.1 Devices(设备页)

> 〔2026-08-30 定案更新,★为目标设计未实现,详见 specs/2026-08-30-device-identity-network-design.md〕

- 页头:本机 IP chip(点击复制,**45s 后自动清空剪贴板**,A7;★升级为名片——全部网卡地址+公网出口+中继地址打包复制)/ 隐身开关(★语义修正:只约束"被看见",出站探测放行)/ +手动添加设备(★粘名片全文或 IP/IP:port,缺端口补 47600;5s 后 manual-probe-result toast)/ 推送文件(向导,保留)/ ★刷新按钮删除(三通道全自动)。
- 设备网格 DeviceCard;**页面级拖放推送**:dragDrop 事件 → getBoundingClientRect 命中卡片 → online+trusted 才接受 → connect → expandLocalPaths → pushFilesRel。
- DeviceCard:状态徽章(重连中>已连接>在线>离线;★重连中扩展为会话级自动重连的显示,连接记忆制)+ 已配对/待配对(★TrustBroken 即时降级);未信任→"连接";已信任→"浏览"+★"推送"(直开向导步 2 跳过选设备)+推送中⏳+⋮下拉(权限设置:浏览/下载 checkbox + 推送策略 select;★强制走中继排障开关;★"推送文件…"项移除——被直推按钮取代;移除信任两击确认→★先发 TrustBroken);下拉贴底自动向上弹(drop-up);★地址行改通道标签「直连 · 2ms」/「经中继 · 120ms」,点击弹通道面板(16 篇)。
- PairingDialog 常驻(见 §4.1;★配对成功自动连接,重配走双盲对称同意门)。

### 3.2 Browse(浏览页)

- 顶部设备 chips(= online && trusted);点选 → connectDevice + 刷共享区。
- 左栏共享区列表(alias + id 前 8 位)+ 刷新;右栏文件列表 + 面包屑(↑上级/根段/逐段跳层,最后段不可点)+ checkbox 多选 + cursor 分页"加载更多" + 文件夹行 hover「下载」(整棵直拉)。
- 底部:已选 N + 下载选中(按 is_dir 分流 start_download/start_download_dir)。
- 联动:remote-shares-changed → 500ms 防抖**静默刷新**(空闲才刷);connection-state 断开提示"列表可能不是最新"。
- 错误"响应通道已被占用" → "传输进行中,请稍后再试"。

### 3.3 Transfers(传输页)

- 列表 TransferItem;页头刷新 + 清除已完成/失败(返回条数 toast)。
- **TransferItem 三角色分派**(local_role):
  - destination:pending 文案 / active 暂停取消 / paused 续取消 / failed 重试(lastRequest 重放)/interrupted 续传(resumePending)/ done **打开所在文件夹**(Rust open_download_dir)+删除;
  - source-push:pending"等待对方确认" / failed 重发(仅 push-rel);
  - source-pull:active **限速菜单**(1/2/4 流/不限,v-click-outside)+ 踢人(confirm+cancel)。
- 展示:角色/状态徽章、进度条(4Hz 间 0.28s 线性补间,终态无动画)、速度、已用时间(**终态冻结** finished_at_ms)、ETA(0 显示"即将完成")、active 健康面板(丢包/RTT/cwnd/流数/remote_done);失败原因经 **friendlyError 脱敏**(盘符/UNC 路径→主文件名,A8)。
- 删除:hasParts 查磁盘残留 → confirm"不可恢复" → deleteParts=true。

### 3.4 Settings(设置页)

7 张卡(auto-fill 网格,中继卡跨全宽):
1. 共享区:addShare(目录选择+别名 prompt)/ removeShare(confirm);
2. 下载目录:展示 + 修改(目录选择);
3. 本机身份:设备名内联编辑 / 指纹缩略 + 短码;
4. 信任设备:displayName(别名优先)/ 备注输入(setAlias,空=清)/ 指纹 + 配对时间 / 权限只读展示 / 移除(confirm"需要重新配对");
5. 防火墙/网络体检:规则状态 / 本机 IP / 三 profile 开关 / 手动 netsh 命令 / 添加规则(UAC)/ 打开日志;
6. 中继(跨全宽):启用 / 服务器 / PSK(password)/ 保存并连接;状态行(已连接含远程设备数/配置错误/连接中/未启用),relay-state 事件 + relayStatus 拉取双驱动;
7. 连接安全:同意超时 / 推送确认超时(15-600 clamp)。

## 4. 组件

### 4.1 PairingDialog(配对对话框,显式状态机)

```
acceptor/gate(同意门,60s 倒计时,到 0 自动 deny)
  └ 同意 → acceptor/code(CodeBadge 亮码 + 结束等待)   [B 侧]
initiator/waiting(可取消)
  └ 收 ConsentGrant → initiator/entry(6 位数字输入)
      → 提交 → initiator/submitted → 结果             [A 侧]
```
- 同意用返回 own_code **乐观更新**,pairing-code-shown 事件兜底覆盖;
- 码错(不匹配)在 submitted 态回 entry 允许重输;其他失败关弹窗+toast;
- canClose 仅 waiting/code;事件监听脚本顶层注册,数组+isUnmounted 双保险防卸载竞态泄漏(v0.11.0)。

### 4.2 PushWizard(推送向导)

步 1 选设备(单选,仅 online+trusted,远程徽章)→ 步 2 选文件(多选文件/文件夹,均 expandLocalPaths 展开,去重合并,可逐项移除)→ 发送(connect → pushFilesRel → record)。卡片"推送文件…"带 presetFingerprint 直跳步 2。

### 4.3 CodeBadge

6 位码 2 位分组空格 + padStart;等宽 48px 紫渐变。

## 5. Stores(全部事件驱动,无轮询)

| store | state | 更新来源 |
|---|---|---|
| devices | devices/pairingPending/selected_fp/reconnecting Set/loading/error | device-list 整表替换;**connection-state 即时修补 connected**(不等 5s);peer-reconnecting 增删重连集合 |
| transfers | transfers/resumeJobs/lastRequest Map(重试参数) | **transfer-progress 事件 250ms(4Hz)防抖合并**(首帧立即上屏,期间暂存,定时器落最后一批);transferAction 乐观本地改态 |
| settings | config/trustedPeers | 拉取(get_settings+list_trusted)+ 动作后本地合并 |
| toast | toasts | push 3s 自动消失;监听 Rust toast 事件 |

## 6. api.ts 调用层

- `invokeCommand<T>`:invoke + **15s 客户端超时兜底**(Rust 侧已有 10s 服务端超时,双保险,防 IPC 挂死永久 loading);catch → console.warn + friendlyError 脱敏。
- 参数命名:Tauri 2 要求 JS camelCase(如 peerCode → peer_code)。
- 分组:devicesApi / pairingApi / browseApi / transfersApi / settingsApi / systemApi + 裸 respondDelete / open_download_dir。

## 7. 事件监听总表(前端 ← Rust)

device-list / connection-state / peer-reconnecting / transfer-progress / toast / pairing-*(5 个) / offer-request(App)/ delete-request(App)/ remote-shares-changed(Browse)/ relay-state(Settings)/ manual-probe-result(Devices)/ onCloseRequested(useCloseGuard)。

## 8. 样式体系(design.css)

唯一 token 源:字体(sans/mono)、primary 蓝阶、语义色 success/warning/danger、冷灰阶、**传输三角色色**(destination 绿/source-push 琥珀/source-pull 橙)、圆角 3 档、4px 基间距、控件高 32/36/44、阴影、字号 12-22、动效(150ms/250ms)。全局:tabular-nums、:focus-visible 焦点环、.btn-*/.card 通用类、prefers-reduced-motion 全关。

## 9. Composables / Directives / 测试

- useCloseGuard:关闭拦截 + prepare_shutdown 语义(浏览器静默降级)。
- useOfferCountdown:500ms 心跳倒计时,≤10s urgent,0 expired 停表;reset(顺延后重新武装)。
- v-click-outside(capture):限速菜单等。
- 测试(Vitest):PairingDialog 状态机 / PushWizard 过滤与直跳 / TransferItem 20 用例(三角色×状态按钮矩阵、健康面板、计时冻结)/ Transfers 页 / transfers store / useCloseGuard / useOfferCountdown / tauriMock(浏览器演示注入)。
