# Task 18 审计报告:秒传位图化 + respond_offer save_dir 目录门禁(阶段三收官)

**Commit:** fc92a66 `refactor(security): 秒传 skip 位图化 + respond_offer save_dir 目录门禁`

## 变更明细

### 1. 秒传 skip 位图化(A5 oracle 完整方案)

- `crates/localtrans-core/src/protocol.rs`:`ControlMsg::OfferResp.skip: Vec<String>` →
  `skip_bitmap: Vec<bool>`,与请求 files 等长逐位对应,true=发送方可跳过。serde default +
  空省略不变(老 JSON 兼容解析)。
- 接收方 `engine.rs::handle_offer_decision`:仅 accepted 时按收件箱索引(hash+size 双校验)
  计算逐位位图;拒绝回空位图。响应中不再出现任何 hash 字符串。
- 发送方 `push_files_inner`:原按 hash 集合匹配改为按请求索引查位图;位图缺位时保守发送
  (不误剔)。大文件路径不受影响(MetaResp 自判定)。
- 接收侧批流剔除/秒传本地复用(`small`/`small_instant`)同步改用 `enumerate + 位图`,
  保留"旧对端不回位图时接收侧不重复落盘"防线。

### 2. respond_offer save_dir 目录门禁(A3 路径门禁延伸)

- `src-tauri/src/commands.rs` 新增 `ensure_save_dir_authorized(save_dir, download_dir)`:
  - canonicalize 只对存在路径成功——沿祖先链 pop 找最近真实存在目录后规范化,
    支持"另存到尚未创建的子目录"场景;
  - 前缀比较分隔符感知且大小写不敏感,防 `downloads-evil` 同名前缀绕过;
  - 下载目录本身/其子目录放行,其余一律 Err("仅允许保存到下载目录内")。
- FFI(Android)`respond_offer(job_id, accept)` 无 save_dir 参数(inbox 由 core 决定),
  无需同校验;OfferResp 协议字段不进 uniffi 接口面,**绑定无需重生成**。

## 测试(TDD)

| 测试 | 结果 |
|---|---|
| 新增 e2e `offer_accept_skip_bitmap_matches_request_order`(2/3 命中 → `[true,true,false]` 按请求顺序) | PASS |
| 更新 `offer_deny_returns_empty_skip` → 空 skip_bitmap 断言 | PASS |
| 新增单测 `save_dir_inside_download_dir_allowed`(含未创建子目录) | PASS |
| 新增单测 `save_dir_outside_download_dir_rejected`(含 downloads-evil 前缀陷阱) | PASS |
| 既有秒传 e2e `push_same_{small,large}_file_second_is_instant` | PASS |

## 阶段三收官门禁:四 crate 全量汇总

| Crate | 结果 |
|---|---|
| localtrans-core(lib) | 197 tests,196 passed / **1 failed(pre-existing,与本批无关)** |
| localtrans-ffi(lib) | 39 passed |
| localtrans-relay(lib) | 27 passed |
| src-tauri localtrans(bin) | 21 passed |
| PC 壳 `cargo build` | OK |
| Android `gradle compileReleaseKotlin` | BUILD SUCCESSFUL |

**Core 失败项说明:** `push_large_file_reports_remote_done`
(remote_done 终值 83886080 vs 期望 104857723,恰为一个 16MB 窗口边界差)。
已在干净工作树(`git stash` 后)复现同样失败——存量 flaky/断言精度问题,
非 T18 引入。三次重跑均稳定失败,建议阶段四专项核查 RecvProgress 终窗确认时序。

## 阶段三台账(T16-T18)

| Task | 内容 | Commit | 状态 |
|---|---|---|---|
| T16 | SaveDir 校验前移等中危修复(save_dir 门禁前置部分) | 阶段三分批提交 | 完成 |
| T17 | 中继/quinn 载荷与超时修复(f872bd4/2b35a5a/09071b5 等) | 分批提交 | 完成 |
| T18 | 秒传 skip 位图化(hash oracle 根除)+ save_dir 目录门禁 | fc92a66 | 完成 |

阶段三(M 级安全/稳定性修复)至此收官。遗留:上表 pre-existing flaky 一项转阶段四。
