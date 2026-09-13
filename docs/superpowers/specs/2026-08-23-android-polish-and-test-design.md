# 安卓 UI Polish 收尾 + 逻辑件 TDD — 设计文档

日期:2026-08-23
来源:gstack design-review 审计(报告:`~/.gstack/projects/localTrans/designs/design-audit-20260823/design-audit-android.md`)
基线:v0.8.0(commit `bbc1247` 之后,F-001~F-007 已修)

## 背景与目标

v0.8.0 完成五项功能需求后,design-review 审计出 7 项发现已全部修复(4 个 style 提交)。
本 spec 收尾两类遗留:

1. **视觉 Polish 项**(审计中标注 Polish 级、未动的布局微调)
2. **3 个纯函数逻辑件补 TDD 测试**(本轮修复中引入、尚未有测试覆盖)

**不做**:Paparizza/Robolectric 截图回归测试等新依赖链;主题体系重构;暗色模式。

## Part A:逻辑件 TDD(先做)

### A1. 抽取 `FailReasons.kt`

从 `TransfersScreen.kt` 抽出两个 private 顶层函数到新文件
`android/app/src/main/java/com/localtrans/app/ui/transfers/FailReasons.kt`(internal 可见性,供同模块测试):

```kotlin
package com.localtrans.app.ui.transfers

/**
 * FFI 层失败原因英文串 → 中文显示(未知串原样返回,Rust 侧部分 reason 已是中文)
 */
internal fun localizeFailReason(reason: String): String = when {
    reason == "removed" -> "已手动移除"
    reason.contains("refused") -> "对方拒绝了本次传输"
    reason.contains("timeout", ignoreCase = true) -> "等待超时"
    reason.contains("cancelled", ignoreCase = true) || reason.contains("canceled", ignoreCase = true) -> "传输已取消"
    reason.contains("disk full", ignoreCase = true) -> "存储空间不足"
    reason.isNotEmpty() -> reason
    else -> "传输失败"
}

/**
 * 可重试的失败类型:对端拒收 / 超时(removed 显式排除)
 */
internal fun isRetryableFailure(reason: String): Boolean {
    if (reason == "removed") return false
    return reason.contains("refused") ||
        reason.contains("timeout") ||
        reason.contains("超时") ||
        reason.contains("拒绝")
}
```

测试文件 `android/app/src/test/java/com/localtrans/app/ui/transfers/FailReasonsTest.kt`,用例:

**localizeFailReason:**
- `"removed"` → `"已手动移除"`
- `"offer refused by peer"` → `"对方拒绝了本次传输"`
- `"timeout"` / `"MetaTimeout"`(大小写不敏感)→ `"等待超时"`
- `"cancelled by peer"` / `"canceled"` → `"传输已取消"`
- `"disk full"` → `"存储空间不足"`
- 中文串透传:`"对方拒绝连接"` → 原样
- 空串 → `"传输失败"`

**isRetryableFailure:**
- `"removed"` → false(手动移除不可重试)
- `"offer refused"` → true
- `"等待超时"` → true(中文也匹配)
- `"disk full"` → false
- 空串 → false

### A2. 抽取 `isTimeoutInvalid()`

从 `SettingsScreen.kt` 内联表达式抽出:

```kotlin
package com.localtrans.app.ui.settings

/** 超时输入校验:非数字或超出 15-600 范围即无效 */
internal fun isTimeoutInvalid(text: String): Boolean {
    val v = text.toIntOrNull() ?: return true
    return v < 15 || v > 600
}
```

测试 `SettingsViewModelTest.kt` 同目录新增 `TimeoutValidationTest.kt`:
- `"60"` → false
- `""` / `"abc"` → true(null 安全)
- `"14"` / `"601"` → true(边界外)
- `"15"` / `"600"` → false(边界内)

### TDD 流程

每件:先写测试(红)→ 抽函数/迁移 → 测试绿 → commit(`test:` / `refactor:` 前缀)。

## Part B:视觉 Polish(后做,截图验证)

| ID | 页面 | 现状 | 目标 | 验证 |
|----|------|------|------|------|
| P-1 | 传输页空态 | 64dp 图标 + 16dp 间距,图标与文字权重失衡 | 图标 56dp,间距 12dp;主标题 bodyLarge→titleMedium | 截图 |
| P-2 | 远程设备选择器空态 | 同上比例问题 | 同 P-1;标题改「选择远程设备」更贴场景 | 截图 |
| P-3 | 设备页空态 | 同上 | 同 P-1 | 截图 |
| P-4 | 设置页范围提示 | 「范围: 15-600 秒」bodySmall 黑字 | labelSmall + onSurfaceVariant(辅助信息弱化) | 截图 |
| P-5 | 设备页指纹卡 | 指纹标题与值间距 8dp、值与开关行 16dp,层次松 | 标题 labelMedium 灰、值 headlineSmall→titleLarge、间距 4/12dp 收紧 | 截图 |
| P-6 | 面包屑行 | 与列表间距 0(紧贴) | 底部加 4dp padding | 截图 |

统一规约:辅助说明文字用 `onSurfaceVariant`(不再用 `.copy(alpha=0.7f)` 叠加——审计指出对比度不足)。
涉及文件:TransfersScreen / FilesScreen / DevicesScreen / SettingsScreen 的空态与卡片组件。

### B 的验收

- 每项独立 commit(`style(design): P-N …`)
- 终轮:assembleRelease → 装模拟器 → 四页 + 远程 Tab 截图,与 `design-audit-20260823/screenshots/` 基线对比
- 单测全绿(Part A 测试不受影响)

## 风险与回退

- 全部为 UI 层改动,协议/FFI 零变更,单端升级即可
- 视觉项如截图复核发现劣化 → `git revert` 对应单 commit(与 design-review 8e 规则一致)

## 执行顺序

1. A1 测试(红)→ 抽取(绿)→ commit
2. A2 测试(红)→ 抽取(绿)→ commit
3. B 各项独立改 + commit
4. 终轮装机截图验证
