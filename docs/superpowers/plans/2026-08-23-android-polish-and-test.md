# 安卓 UI Polish 收尾 + 逻辑件 TDD 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落地 spec `docs/superpowers/specs/2026-08-23-android-polish-and-test-design.md` 的两部分:① 两个 UI 纯逻辑件以 TDD 抽取并补测试(FailReasons / isTimeoutInvalid);② 6 项视觉 Polish(空态比例、辅助文字、指纹卡、面包屑)以截图验证收尾。

**Architecture:** 纯 Kotlin UI 层改动,协议/FFI/Rust 零变更。逻辑件从 Compose Screen 文件抽到同包普通 Kotlin 文件(internal 可见性),单测走 JUnit4(工程既有 test sourceSet,无新依赖)。视觉项每项独立 commit,终轮 assembleRelease + adb 装模拟器截图与基线对比。

**Tech Stack:** Kotlin 1.9 / Jetpack Compose Material 3 / JUnit4 / Gradle 8.10.2(--offline)/ adb + emulator-5554

## Global Constraints

- 提交信息中文,前缀按类型:`test:` / `refactor:` / `style(design):`,结尾必须带 `Co-Authored-By: Claude <noreply@anthropic.com>`
- Gradle 命令一律:`export JAVA_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8"` + `cd /c/Users/<user>/Desktop/work/localTrans/android` + `C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin/gradle.bat <task> --offline`
- 单测命令:`...gradle.bat :app:testReleaseUnitTest --offline -q`(全绿标准:无 FAILED 输出;`-q` 下无输出即全绿)
- 协议/FFI/Rust 层零变更,单端升级即可
- 辅助说明文字颜色统一用 `MaterialTheme.colorScheme.onSurfaceVariant`,禁止 `.copy(alpha = 0.7f)` 叠加(spec 规约)
- 视觉项如终轮截图复核发现劣化 → `git revert HEAD` 单 commit 回退
- 每个任务完成后在 `.superpowers/sdd/progress.md` 追加一行 ledger

---

### Task 1: FailReasons 抽取 + TDD(spec Part A / A1)

**Files:**
- Create: `android/app/src/main/java/com/localtrans/app/ui/transfers/FailReasons.kt`
- Modify: `android/app/src/main/java/com/localtrans/app/ui/transfers/TransfersScreen.kt`(删除 328-363 行附近两个 private 函数与文档注释)
- Test: `android/app/src/test/java/com/localtrans/app/ui/transfers/FailReasonsTest.kt`(新建)

**Interfaces:**
- Consumes: 无(首个任务)
- Produces: `internal fun localizeFailReason(reason: String): String` 与 `internal fun isRetryableFailure(reason: String): Boolean`,包 `com.localtrans.app.ui.transfers`。Task 3(终轮)不依赖此件,但 TransfersScreen 后续所有失败原因显示都经这两个函数。

- [ ] **Step 1: 写失败测试**

创建 `android/app/src/test/java/com/localtrans/app/ui/transfers/FailReasonsTest.kt`:

```kotlin
package com.localtrans.app.ui.transfers

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class FailReasonsTest {

    // ---- localizeFailReason ----

    @Test
    fun `removed maps to manual removal copy`() {
        assertEquals("已手动移除", localizeFailReason("removed"))
    }

    @Test
    fun `refused maps to peer rejected copy`() {
        assertEquals("对方拒绝了本次传输", localizeFailReason("offer refused by peer"))
    }

    @Test
    fun `timeout is case insensitive`() {
        assertEquals("等待超时", localizeFailReason("timeout"))
        assertEquals("等待超时", localizeFailReason("MetaTimeout"))
    }

    @Test
    fun `cancelled and canceled both map to cancelled copy`() {
        assertEquals("传输已取消", localizeFailReason("cancelled by peer"))
        assertEquals("传输已取消", localizeFailReason("canceled"))
    }

    @Test
    fun `disk full maps to storage copy`() {
        assertEquals("存储空间不足", localizeFailReason("disk full"))
    }

    @Test
    fun `unknown chinese reason passes through`() {
        assertEquals("对方拒绝连接", localizeFailReason("对方拒绝连接"))
    }

    @Test
    fun `empty reason falls back to generic failure`() {
        assertEquals("传输失败", localizeFailReason(""))
    }

    // ---- isRetryableFailure ----

    @Test
    fun `removed is not retryable`() {
        assertFalse(isRetryableFailure("removed"))
    }

    @Test
    fun `refused is retryable`() {
        assertTrue(isRetryableFailure("offer refused"))
    }

    @Test
    fun `chinese timeout keyword is retryable`() {
        assertTrue(isRetryableFailure("等待超时"))
    }

    @Test
    fun `disk full is not retryable`() {
        assertFalse(isRetryableFailure("disk full"))
    }

    @Test
    fun `empty reason is not retryable`() {
        assertFalse(isRetryableFailure(""))
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `export JAVA_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8" && cd /c/Users/<user>/Desktop/work/localTrans/android && C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin/gradle.bat :app:testReleaseUnitTest --offline -q`
Expected: FAIL,编译错误 `unresolved reference: localizeFailReason`(函数还在 TransfersScreen.kt 是 private,测试不可见)

- [ ] **Step 3: 抽取实现**

创建 `android/app/src/main/java/com/localtrans/app/ui/transfers/FailReasons.kt`(内容从 TransfersScreen.kt 原样迁移,private → internal):

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

同时删除 TransfersScreen.kt 中原有的两个 private 函数(约 328-363 行,含各自的 KDoc 注释)。Screen 内调用点(约 252 行 `localizeFailReason(...)` 与约 302 行 `isRetryableFailure(...)`)不需要改动——同包顶层函数直接可见。

- [ ] **Step 4: 跑测试确认通过**

Run: 同 Step 2 命令
Expected: PASS(无输出)。新增 12 个用例全绿,既有测试无回归。

- [ ] **Step 5: Commit**

```bash
cd /c/Users/<user>/Desktop/work/localTrans
git add android/app/src/main/java/com/localtrans/app/ui/transfers/FailReasons.kt \
        android/app/src/main/java/com/localtrans/app/ui/transfers/TransfersScreen.kt \
        android/app/src/test/java/com/localtrans/app/ui/transfers/FailReasonsTest.kt
git commit -m "test: FailReasons 抽取 + 12 用例(失败原因中文映射/重试判定)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 2: isTimeoutInvalid 抽取 + TDD(spec Part A / A2)

**Files:**
- Create: `android/app/src/main/java/com/localtrans/app/ui/settings/TimeoutValidation.kt`
- Modify: `android/app/src/main/java/com/localtrans/app/ui/settings/SettingsScreen.kt:42-43`(两个内联表达式改调新函数)
- Test: `android/app/src/test/java/com/localtrans/app/ui/settings/TimeoutValidationTest.kt`(新建)

**Interfaces:**
- Consumes: 无
- Produces: `internal fun isTimeoutInvalid(text: String): Boolean`,包 `com.localtrans.app.ui.settings`。

- [ ] **Step 1: 写失败测试**

创建 `android/app/src/test/java/com/localtrans/app/ui/settings/TimeoutValidationTest.kt`:

```kotlin
package com.localtrans.app.ui.settings

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TimeoutValidationTest {

    @Test
    fun `normal value is valid`() {
        assertFalse(isTimeoutInvalid("60"))
    }

    @Test
    fun `empty string is invalid`() {
        assertTrue(isTimeoutInvalid(""))
    }

    @Test
    fun `non numeric is invalid`() {
        assertTrue(isTimeoutInvalid("abc"))
    }

    @Test
    fun `below minimum is invalid`() {
        assertTrue(isTimeoutInvalid("14"))
    }

    @Test
    fun `above maximum is invalid`() {
        assertTrue(isTimeoutInvalid("601"))
    }

    @Test
    fun `boundaries are valid`() {
        assertFalse(isTimeoutInvalid("15"))
        assertFalse(isTimeoutInvalid("600"))
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: 同 Task 1 Step 2 命令
Expected: FAIL,`unresolved reference: isTimeoutInvalid`

- [ ] **Step 3: 写实现并接线**

创建 `android/app/src/main/java/com/localtrans/app/ui/settings/TimeoutValidation.kt`:

```kotlin
package com.localtrans.app.ui.settings

/** 超时输入校验:非数字或超出 15-600 范围即无效 */
internal fun isTimeoutInvalid(text: String): Boolean {
    val v = text.toIntOrNull() ?: return true
    return v < 15 || v > 600
}
```

修改 `SettingsScreen.kt` 第 42-43 行:

```kotlin
// 原:
    val offerTimeoutInvalid = form.offerTimeoutSecs.toIntOrNull()?.let { it < 15 || it > 600 } ?: false
    val consentTimeoutInvalid = form.consentTimeoutSecs.toIntOrNull()?.let { it < 15 || it > 600 } ?: false
// 改为:
    val offerTimeoutInvalid = isTimeoutInvalid(form.offerTimeoutSecs)
    val consentTimeoutInvalid = isTimeoutInvalid(form.consentTimeoutSecs)
```

注意语义变化:原内联表达式对非数字输入返回 false(不标红),新函数返回 true(标红)。这是 spec 明确的预期行为("非数字或超出范围即无效"),不是回归。

- [ ] **Step 4: 跑测试确认通过**

Run: 同 Task 1 Step 2 命令
Expected: PASS(无输出)。新增 6 用例全绿;SettingsViewModelTest 3 个既有用例无回归。

- [ ] **Step 5: Commit**

```bash
cd /c/Users/<user>/Desktop/work/localTrans
git add android/app/src/main/java/com/localtrans/app/ui/settings/TimeoutValidation.kt \
        android/app/src/main/java/com/localtrans/app/ui/settings/SettingsScreen.kt \
        android/app/src/test/java/com/localtrans/app/ui/settings/TimeoutValidationTest.kt
git commit -m "test: isTimeoutInvalid 抽取 + 6 边界用例(非数字/越界标红)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 3: 空态视觉统一(P-1/P-2/P-3)

**Files:**
- Modify: `android/app/src/main/java/com/localtrans/app/ui/transfers/TransfersScreen.kt`(EmptyTransfers,约 121-150 行)
- Modify: `android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt`(RemoteDevicePicker 空态,约 877-898 行)
- Modify: `android/app/src/main/java/com/localtrans/app/ui/devices/DevicesScreen.kt`(EmptyState,约 290-324 行)

**Interfaces:**
- Consumes: 无
- Produces: 无(纯视觉)

- [ ] **Step 1: 改传输页空态 EmptyTransfers**

```kotlin
@Composable
private fun EmptyTransfers() {
    Box(
        modifier = Modifier.fillMaxSize(),
        contentAlignment = Alignment.Center
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center
        ) {
            Icon(
                imageVector = Icons.Default.SwapHoriz,
                contentDescription = null,
                modifier = Modifier.size(56.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant
            )
            Spacer(modifier = Modifier.height(12.dp))
            Text(
                text = "暂无传输任务",
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant
            )
            Spacer(modifier = Modifier.height(4.dp))
            Text(
                text = "从文件页选择文件发送,或浏览远程文件拉取",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant
            )
        }
    }
}
```

变化:64dp→56dp、16dp→12dp 间距、bodyLarge→titleMedium、副文案去掉 `.copy(alpha = 0.7f)`。

- [ ] **Step 2: 改设备页空态 EmptyState(同样参数)**

DevicesScreen.kt 的 EmptyState:图标 `Modifier.size(64.dp)` → `Modifier.size(56.dp)`;`Spacer(Modifier.height(16.dp))` → `12.dp`;副文案「确保设备在同一网络且应用已启动」的 color 去掉 `.copy(alpha = 0.7f)`。

- [ ] **Step 3: 改远程选择器空态 + 标题**

FilesScreen.kt RemoteDevicePicker:空态图标 48dp→56dp、12dp→12dp 间距保持、副文案去掉 alpha;标题「选择要浏览的设备」→「选择远程设备」。

- [ ] **Step 4: 编译验证**

Run: `export JAVA_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8" && cd /c/Users/<user>/Desktop/work/localTrans/android && C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin/gradle.bat :app:compileReleaseKotlin --offline -q`
Expected: 无输出(编译通过)

- [ ] **Step 5: Commit**

```bash
cd /c/Users/<user>/Desktop/work/localTrans
git add android/app/src/main/java/com/localtrans/app/ui/transfers/TransfersScreen.kt \
        android/app/src/main/java/com/localtrans/app/ui/devices/DevicesScreen.kt \
        android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt
git commit -m "style(design): P-1/2/3 三处空态视觉统一(56dp 图标/层级收紧/去半透明)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 4: 辅助文字弱化 + 指纹卡收紧 + 面包屑间距(P-4/P-5/P-6)

**Files:**
- Modify: `android/app/src/main/java/com/localtrans/app/ui/settings/SettingsScreen.kt`(范围提示,约 71-75 行)
- Modify: `android/app/src/main/java/com/localtrans/app/ui/devices/DevicesScreen.kt`(MyFingerprintSection,约 78-129 行)
- Modify: `android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt`(BreadcrumbRow,约 610-643 行)

**Interfaces:**
- Consumes: 无
- Produces: 无(纯视觉)

- [ ] **Step 1: 设置页范围提示弱化(P-4)**

```kotlin
// 原(约 71-75 行):
                    Text(
                        text = "范围: 15-600 秒",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
// 改为:
                    Text(
                        text = "范围: 15-600 秒",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
```

- [ ] **Step 2: 指纹卡收紧(P-5)**

DevicesScreen.kt MyFingerprintSection 内:

```kotlin
// 标题行:titleSmall → labelMedium
            Text(
                text = "我的设备指纹",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSecondaryContainer
            )

            Spacer(modifier = Modifier.height(4.dp))   // 8dp → 4dp

// 指纹值:headlineSmall → titleLarge,颜色改 onSecondaryContainer(原 primary 与卡片语义冲突)
            Text(
                text = if (fingerprint.length >= 8) fingerprint.take(8) else "...",
                style = MaterialTheme.typography.titleLarge,
                color = MaterialTheme.colorScheme.onSecondaryContainer
            )

            Spacer(modifier = Modifier.height(12.dp))  // 16dp → 12dp
```

- [ ] **Step 3: 面包屑底部间距(P-6)**

FilesScreen.kt BreadcrumbRow 的 Row: `modifier = Modifier.padding(8.dp)` → `modifier = Modifier.padding(start = 8.dp, end = 8.dp, top = 8.dp, bottom = 12.dp)`

- [ ] **Step 4: 编译 + 全量单测**

Run: `:app:compileReleaseKotlin` 与 `:app:testReleaseUnitTest`(命令同 Task 1 Step 2)
Expected: 均无输出(通过)

- [ ] **Step 5: Commit**

```bash
cd /c/Users/<user>/Desktop/work/localTrans
git add android/app/src/main/java/com/localtrans/app/ui/settings/SettingsScreen.kt \
        android/app/src/main/java/com/localtrans/app/ui/devices/DevicesScreen.kt \
        android/app/src/main/java/com/localtrans/app/ui/files/FilesScreen.kt
git commit -m "style(design): P-4/5/6 范围提示弱化+指纹卡收紧+面包屑间距

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 5: 终轮装机截图验证(spec Part B 验收)

**Files:**
- 无源码修改(验证任务;劣化则 revert 对应 commit)

**Interfaces:**
- Consumes: Task 1-4 的全部产物
- Produces: 验证结论 + 截图存档

- [ ] **Step 1: 打 release APK**

Run: `export JAVA_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/jdk17/jdk-17.0.20+8" && cd /c/Users/<user>/Desktop/work/localTrans/android && C:/Users/<user>/Desktop/work/deepseek_use/tools/gradle-8.10.2/bin/gradle.bat :app:assembleRelease --offline -q`
Expected: `android/app/build/outputs/apk/release/app-release.apk` 更新

- [ ] **Step 2: 装模拟器并冷启动**

```bash
ADB="C:/Users/<user>/Desktop/work/deepseek_use/tools/android-sdk/platform-tools/adb.exe"
cd /c/Users/<user>/Desktop/work/localTrans
"$ADB" -s emulator-5554 install -r android/app/build/outputs/apk/release/app-release.apk
"$ADB" -s emulator-5554 shell am force-stop com.localtrans.app
sleep 1
"$ADB" -s emulator-5554 shell am start -n com.localtrans.app/.MainActivity
sleep 6
"$ADB" -s emulator-5554 shell pidof com.localtrans.app   # 必须输出 pid
"$ADB" -s emulator-5554 logcat -d -s AndroidRuntime:E    # 必须无 FATAL
```

注意:adb 路径必须从仓库根目录给相对路径或绝对 Windows 装机路径(在 android/ 目录下 install 会报 failed to stat)。

- [ ] **Step 3: 截四页 + 远程 Tab 并复核**

模拟器 720x1280,底部导航 y≈1232,四 tab 中心 x = 90/267/451/635;文件页顶部「本机/远程」一级 Tab 远程中心约 (540, 95)。

```bash
snap() {  # 参数:输出文件名(不含扩展名)
  "$ADB" -s emulator-5554 shell screencap -p "//sdcard//s.png"   # 双斜杠防 Git Bash 路径转换
  "$ADB" -s emulator-5554 pull "//sdcard//s.png" "$REPORT_DIR/screenshots/$1.png"
}
```

逐张截图:设备页(冷启动即达)→ 文件页(x=267)→ 远程 Tab(540,95)→ 传输页(451)→ 设置页(635),命名 `polish-{page}.png` 存 `~/.gstack/projects/localTrans/designs/design-audit-20260823/screenshots/`。

复核清单(逐张对照):
- 传输空态:图标明显小于之前、标题更大、副文案更清晰
- 远程空态:标题为「选择远程设备」
- 设备页指纹卡:标题变小灰字、指纹值缩小、整卡更紧凑
- 设置页:范围提示变小
- 全部页面无 FATAL、无布局错位

- [ ] **Step 4: 收尾 ledger + 汇报**

```bash
echo "Task 1-5: complete" >> /c/Users/<user>/Desktop/work/localTrans/.superpowers/sdd/progress.md
```

汇报内容:5 commit 列表、单测 61 用例(49 既有 + 12 新增 FailReasons)全绿、装机验证结论、如有劣化项列出 revert 建议。
