# N1 夜间批次：传输域收尾 + 浏览页定案 + 跨端 E2E

> 2026-09-06 夜。前提：E2E 工具链已全量验收（13/13，见 docs/e2e-harness.md）；设计定案见 docs/design/15§④§⑥ 与 specs/2026-08-30-browse-page-design.md。
> 方法：superpowers 逐任务 TDD（先失败测试→实现→gate 绿→原子提交）；涉及传输域的修完必跑真机 E2E 复验。

## 范围与优先级

| 任务 | 泳道 | 内容 | 验收 |
|---|---|---|---|
| T1 | shell | 单文件推送发送端多卡（failed 占位 + done×2，违反"恒一张父卡"定案） | 失败单测锚定 + e2e 推送后发送端恒 1 卡 |
| T2 | shell | 取消后引擎槽位泄漏（下次推送卡 pending，违反出队语义） | 失败单测锚定 + e2e 取消→再推不 pending |
| T3 | ffi | pull_files 多选只拉第一个（lib.rs 两分支相同） | 单测多文件断言 |
| T4 | shell+ui | 浏览页批次下载：N 选文件 → 1 批次 → 传输页 1 张父卡片（复用 batch/children 机制） | UI 单测 + e2e 多选下载见 1 父卡 |
| T5 | ui | 浏览页本地排序（名称/大小/时间+升降，目录优先）+ 删刷新按钮×3（设备页 1+浏览页 2） | vitest + 手动截图目检 |
| T6 | e2e | M8-a：PC↔Android 真实传输场景（配对→推送→双端终态→截图） | 场景 PASS |
| T7 | 余量 | M8-b 中继场景 + L3 出厂重置；或 P0-1 测试端口可配置化 | 视余量 |

## 纪律

- 改哪个泳道跑哪条 gate（core/shell/ui）；传输域修复必配 E2E 真机复验（deploy→场景→截图）。
- 日志红线/机密红线照 AGENTS.md；提交直接进 main（项目惯例），不 push。
- 每任务完成在本文末尾追加执行记录行。

## 执行记录

| 任务 | 状态 | 证据 |
|---|---|---|
| T1 单文件推送多卡 | **完成** | 根因三层:①全秒传跳过时引擎不发 Started(core 补配对,新单测 full_skip_push_still_emits_started_done_pair);②source job 无登记致 source 桥另建卡(single_peer_key+source_push_attach+bind_engine_id_passive,新单测);提交 eebcc9c+d39856f;探针真机四轮验证恒 1 卡 |
| T2 取消后槽位泄漏 | **完成** | 根因:成功路径 select 循环不退出(许可泄漏)+plain push 无取消信号(engine_done 守卫+placeholder_cancels 接通);真机验证取消秒落 failed"对方接收失败:已取消"且再推立即得槽完成 |
| T3 FFI pull_files 多选 | **代码完成** | 逐文件顺序拉取聚合单卡(6e84b66);真机验证被 P0 配对死锁阻塞(手机需与 PC 配对后才能拉取) |
| T4 浏览页批次下载 | **完成** | start_download_batch+handle_batch_pull_event(子项差分计量——首版直接累加父卡双计 60→120,真机抓出后修);night-browse-batch 场景真机 PASS(1 父卡 60/60+B 落盘核对);de31f33 |
| T5 排序+删刷新按钮 | **完成** | sortedFiles(目录优先/三键/升降)+删 3 按钮;真机三序断言+按钮消失断言 PASS;de31f33 |
| T6 PC↔Android 场景 | **受阻→转 P0** | 场景框架成(night-android-transfer.mjs:装包/横幅/自动同意/点卡即连/推 OfferSheet 段);配对环节撞出 **P0:真实配对同意门双向死锁**(TASKS.md P0-6,证据链完整:文件日志入站链断/双机双向/两版二进制/core 单测绿疑壳层锁) |
| T7 | 未开始(余量被 P0 调查消耗) | |
| 附带 | — | 核心门禁串行 209 全绿;并行 flaky 为存量(stash A/B 对照证实,轮转 1-4 个端口类/全局钩子类);push_large 容差放宽 2 窗口(注释自认的时序容差不足);夹具状态:PC↔laptop 信任已直连播种恢复(trusted_peers.json),手机与 PC 未配对(待 P0 修复) |


## 追加执行记录(2026-09-07 晨)

| 任务 | 状态 | 说明 |
|---|---|---|
| P0-6 根因+修复 | **完成** | "配对死锁"真根因=PairingDialog 页面级挂载(离设备页事件失听);提升 App.vue 全局;真机全自动配对闭环验证(同意→读码去空格→手机坐标输码→码匹配→双向信任) |
| 探针排查法 | 完成 | session.rs 六点临时探针定位受信握手各步;已撤除,core 串行 209 绿 |
| T6 推送段 | 受阻→P0-7 | 手机↔PC 握手 ✓ 但 offer 秒死(t+4s 亦不可达,配对期同连接流量正常)——安卓 FFI 侧独立问题,立 P0-7 |
| 工具修复 | 完成 | adb 通道补 tapXY(原始坐标点击——此前 {x,y} 误入选择器路径必失败)与 swipe 导出 |

## 追加执行记录(2026-09-07 上午,P0-7 专项)

| 任务 | 状态 | 说明 |
|---|---|---|
| P0-7 定案 | **完成** | 三点点位探针(ctrl_fwd/router_got/ffi_ask)+裸时窗日志证伪"秒死"假说:OfferReq 全链通、OfferSheet 正常弹出、2MB 推送真机完成落盘(Download/LocalTrans)。此前假象=编排器两缺陷叠加(桥tag过滤漏行+时窗错位;接收按钮 clickable/文本节点分离致匹配恒 miss→60s 自动拒绝→超时→连接闲置死)。探针已全部撤除,core 串行 209 绿 |
| 永久改进 | 完成 | ffi logcat.rs 装 panic 钩子(任务级 panic 进 logcat,00bda2e) |
| T6 推送段 | **PASS** | 真机:拨号→会话→offer→弹窗→点接收→2MB 落盘 |
| T3 批拉 UI 自动化 | 未完成 | 手机文件页多选交互模型未探明+手机长时间运行退化(post_handouse 挂,重启恢复,P2)反复干扰;代码修复(6e84b66)已在,UI 自动化下轮接续 |
