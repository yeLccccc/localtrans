# Android 复制 IP + 手动添加设备 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Android 设备页新增本机信息卡(设备名+IP 一键复制)与手动添加设备弹窗(输 IP → 探测 → 设备入列),与 PC 端能力对齐。

**Architecture:** FFI 加两个薄方法(`local_ip` UDP 选路取本机 IP、`probe_addr` 透传 `DiscoveryCmd::ProbeAddr`),探测结果反馈在 Kotlin 侧做(5s 后重查 devices 按 IP 比对,镜像 PC 壳逻辑),零 core 变更、零 AppEvent 枚举变更。

**Tech Stack:** Rust(uniffi 0.28)/ Kotlin Compose / Material3

**Spec:** docs/superpowers/specs/2026-08-26-android-copyip-manual-add-design.md

## Global Constraints

- 版本:**仅 Android 侧** versionCode 13→14、versionName "0.10.2"→"0.10.3"、CHANGELOG 加 0.10.3 条目;PC/relay 功能零变更,Cargo.toml/tauri.conf.json **不动**(纯 Android+FFI 版,PC zip 沿用 v0.10.2)
- 默认发现端口 **47600**(仅 IP 时补此端口;设备表存 QUIC 端口 47601,比对 IP 用 `substringBefore(':')`)
- 绑定重生成后**必须重做** `localtrans_ffi.kt` 两处 AppException `message→errorMessage` 手工补丁(约 :3228/:3237,v0.8.0 起已知事项)
- 安全规约:目标 IP/指纹不进日志;PSK/配对码规约沿用
- 提交信息:中文前缀 + 空行 + Co-Authored-By: Claude <noreply@anthropic.com>
- Rust 测试 `cargo test -p localtrans-ffi -- --test-threads=1`;Kotlin 测试 `cd android && ./gradlew testDebugUnitTest`;main 分支直接工作
- 环境注:NDK 路径 `C:/Users/<user>/Desktop/work/deepseek_use/tools/android-sdk/ndk/27.0.12077973`(gradle 任务内部已设);FFI 集成测试若 7 失败为用户运行中 exe 占 47600/47601 端口(已知环境非回归)

---

### Task 1: FFI local_ip + probe_addr

**Files:**
- Modify: `crates/localtrans-ffi/src/lib.rs`(在 `set_hidden` 方法后、`connect_device` 前插入两个方法;文件末尾 tests 模块加测试)

**Interfaces:**
- Produces: `LocalTransApp::local_ip(&self) -> Option<String>`(uniffi → Kotlin `fun localIp(): String?`)
- Produces: `LocalTransApp::probe_addr(&self, addr: String) -> Result<(), AppException>`(Kotlin `fun probeAddr(addr: String)`,失败抛 AppException)
- Consumes: `state.discovery.cmd`(AppState 已有字段,`Arc<discovery::DiscoveryHandle>` 的 mpsc Sender)、`DiscoveryCmd::ProbeAddr(SocketAddr)`(core 已有)

- [ ] **Step 1: 写失败测试**(lib.rs 末尾 tests 模块内)

```rust
    #[test]
    fn parse_probe_addr_bare_ip_gets_default_port() {
        let a = parse_probe_addr("192.168.1.100").unwrap();
        assert_eq!(a, "192.168.1.100:47600".parse().unwrap());
    }

    #[test]
    fn parse_probe_addr_ip_port_kept() {
        let a = parse_probe_addr("192.168.1.100:48000").unwrap();
        assert_eq!(a.port(), 48000);
        assert_eq!(a.ip().to_string(), "192.168.1.100");
    }

    #[test]
    fn parse_probe_addr_garbage_errs() {
        assert!(parse_probe_addr("not-an-ip").is_err());
        assert!(parse_probe_addr("").is_err());
        assert!(parse_probe_addr("192.168.1.999").is_err());
    }

    #[test]
    fn udp_local_ip_does_not_panic() {
        // 无网环境返回 None 也算通过——只断言不 panic
        let _ = udp_local_ip();
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-ffi parse_probe -- --test-threads=1`
Expected: 编译失败 `cannot find function parse_probe_addr`

- [ ] **Step 3: 实现**(lib.rs,`set_hidden` 与 `connect_device` 之间)

```rust
    /// 本机主网络 IP(UDP connect 仅本地选路不发包;镜像桌面壳 firewall::primary_local_ip)
    pub fn local_ip(&self) -> Option<String> {
        udp_local_ip()
    }

    /// 手动探测指定地址:仅 IP 补默认发现端口 47600;IP:端口 直用;坏格式 AppException。
    /// 本机隐身时发现层门控自动跳过(不发包),由调用方文案涵盖。
    pub fn probe_addr(&self, addr: String) -> Result<(), AppException> {
        let socket_addr = parse_probe_addr(&addr)
            .map_err(|e| AppException::Io { message: format!("无效地址: {}", e) })?;
        let state_guard = self.state.lock().unwrap();
        match &*state_guard {
            Some(state) => {
                self.runtime.block_on(async {
                    state.discovery.cmd
                        .send(localtrans_core::discovery::DiscoveryCmd::ProbeAddr(socket_addr))
                        .await
                        .map_err(|e| AppException::Io { message: format!("探测失败: {}", e) })
                })
            }
            None => Err(AppException::Io { message: "应用未启动".into() }),
        }
    }
```

文件中(`clean_backup_dir_name` 附近,自由函数区)加:

```rust
/// UDP connect 8.8.8.8 只做本地选路不真正发包,取主网卡 IP
fn udp_local_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:9").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

/// 解析手动探测地址:含冒号按 SocketAddr 直解;仅 IP 补默认发现端口 47600
fn parse_probe_addr(input: &str) -> Result<std::net::SocketAddr, String> {
    let input = input.trim();
    if input.contains(':') {
        input.parse().map_err(|e| format!("{}", e))
    } else {
        let ip: std::net::IpAddr = input.parse().map_err(|e| format!("{}", e))?;
        Ok(std::net::SocketAddr::new(ip, 47600))
    }
}
```

- [ ] **Step 4: 跑测试转绿 + 回归**

Run: `cargo test -p localtrans-ffi -- --test-threads=1`
Expected: 新增 4 测试全过;既有测试与基线一致(端口占用类失败为已知环境)

- [ ] **Step 5: Commit**

```bash
git add crates/localtrans-ffi/src/lib.rs
git commit -m "feat(ffi): local_ip 取本机主网卡 IP + probe_addr 手动探测入口

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 2: so 重编 + 绑定重生成 + errorMessage 补丁

**Files:**
- Regenerate: `android/app/src/main/java/uniffi/localtrans_ffi/localtrans_ffi.kt`
- Rebuild: `android/app/src/main/jniLibs/{arm64-v8a,x86_64}/liblocaltrans_ffi.so`

**Interfaces:**
- Consumes: Task 1 的 `local_ip`/`probe_addr`(重生成后 Kotlin 侧出现 `localIp(): String?`、`probeAddr(addr: String)`)
- Produces: 可在 Kotlin 编译的绑定(含 errorMessage 补丁)

- [ ] **Step 1: 记录补丁现状**

Run: `grep -n "手工修复" android/app/src/main/java/uniffi/localtrans_ffi/localtrans_ffi.kt`
Expected: 两处(约 :3228/:3237),记下上下文 10 行供重做参照

- [ ] **Step 2: so 重编 + 绑定重生成**

Run: `cd android && ./gradlew genUniffi`
(任务内部:buildRustSo 双 ABI cargo ndk + uniffi-bindgen generate;NDK 路径任务内已设)
Expected: BUILD SUCCESSFUL;`grep -n "fun localIp\|fun probeAddr" android/app/src/main/java/uniffi/localtrans_ffi/localtrans_ffi.kt` 命中

- [ ] **Step 3: 重做 errorMessage 手工补丁**

重生成会冲掉补丁导致 AppException 与 Throwable.message 冲突编译不过。在 :3228/:3237 附近(重生成后行号会漂移,按 `class` 搜索)对两处恢复:

```kotlin
        // 手工修复:字段改名 errorMessage 并保留 override message;重生成后需重做。
        val errorMessage: kotlin.String = `message`
        override val message: kotlin.String get() = errorMessage
```

- [ ] **Step 4: 验证编译**

Run: `cd android && ./gradlew :app:compileDebugKotlin`
Expected: BUILD SUCCESSFUL

- [ ] **Step 5: Commit**

```bash
git add android/app/src/main/java/uniffi/ android/app/src/main/jniLibs/
git commit -m "chore(android): 重编 so 并重生成 uniFFI 绑定(含 errorMessage 补丁重做)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 3: Kotlin 数据层 + ViewModel 探测状态机

**Files:**
- Modify: `android/app/src/main/java/com/localtrans/app/data/DevicesRepo.kt`(接口/Ffi/Fake 三处)
- Modify: `android/app/src/main/java/com/localtrans/app/ui/devices/DevicesUiModel.kt`
- Modify: `android/app/src/main/java/com/localtrans/app/ui/devices/DevicesViewModel.kt`
- Test: `android/app/src/test/java/com/localtrans/app/ui/devices/DevicesViewModelTest.kt`、新建 `DevicesUiModelTest.kt`

**Interfaces:**
- Consumes: Task 2 绑定 `app.localIp()` / `app.probeAddr(addr)`
- Produces: `DevicesRepo.localIp()/probeAddr(addr)`;`DevicesUiState.localIp/deviceName/manualProbe`;`viewModel.probeDevice(addr)/clearManualProbe()`;纯函数 `isPrivateLanIp(ip)`

- [ ] **Step 1: 写失败测试**

DevicesUiModelTest.kt(新建):

```kotlin
package com.localtrans.app.ui.devices

import org.junit.Assert.*
import org.junit.Test

class DevicesUiModelTest {
    @Test
    fun `private lan ips detected`() {
        assertTrue(isPrivateLanIp("192.168.1.5"))
        assertTrue(isPrivateLanIp("10.1.2.3"))
        assertTrue(isPrivateLanIp("172.16.0.1"))
        assertTrue(isPrivateLanIp("172.31.255.255"))
        assertTrue(isPrivateLanIp("127.0.0.1"))
    }

    @Test
    fun `public and malformed ips rejected`() {
        assertFalse(isPrivateLanIp("8.8.8.8"))
        assertFalse(isPrivateLanIp("172.32.0.1"))
        assertFalse(isPrivateLanIp("100.64.0.1")) // CGNAT,移动网络典型段
        assertFalse(isPrivateLanIp("abc"))
        assertFalse(isPrivateLanIp(""))
    }
}
```

DevicesViewModelTest.kt 追加(DeviceDto 构造参数以现有测试用法为准):

```kotlin
    @Test
    fun `local ip loaded into state`() = runTest {
        fakeRepo.fakeLocalIp = "10.0.0.5"
        val vm = DevicesViewModel(fakeRepo)
        testDispatcher.scheduler.advanceUntilIdle()
        assertEquals("10.0.0.5", vm.uiState.value.localIp)
    }

    @Test
    fun `probe finds device after delay`() = runTest {
        fakeRepo.addDevice(DeviceDto(fingerprint = "FP1", name = "PC", addr = "192.168.1.100:47601", online = true, connected = false, viaRelay = false))
        viewModel.probeDevice("192.168.1.100")
        assertEquals(ProbeState.PROBING, viewModel.uiState.value.manualProbe?.state)
        testDispatcher.scheduler.advanceTimeBy(5000)
        testDispatcher.scheduler.advanceUntilIdle()
        assertEquals(ProbeState.FOUND, viewModel.uiState.value.manualProbe?.state)
        assertEquals("192.168.1.100", fakeRepo.lastProbedAddr)
    }

    @Test
    fun `probe reports not found when no device matches`() = runTest {
        viewModel.probeDevice("192.168.1.200")
        testDispatcher.scheduler.advanceTimeBy(5000)
        testDispatcher.scheduler.advanceUntilIdle()
        val p = viewModel.uiState.value.manualProbe
        assertNotNull(p)
        assertEquals(ProbeState.NOT_FOUND, p?.state)
        assertTrue(p?.message?.contains("隐身") == true)
    }

    @Test
    fun `probe reports error when send fails`() = runTest {
        fakeRepo.probeError = RuntimeException("boom")
        viewModel.probeDevice("192.168.1.200")
        testDispatcher.scheduler.advanceUntilIdle()
        assertEquals(ProbeState.ERROR, viewModel.uiState.value.manualProbe?.state)
    }

    @Test
    fun `clear manual probe cancels pending job`() = runTest {
        viewModel.probeDevice("192.168.1.200")
        viewModel.clearManualProbe()
        testDispatcher.scheduler.advanceTimeBy(6000)
        testDispatcher.scheduler.advanceUntilIdle()
        assertNull(viewModel.uiState.value.manualProbe)
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd android && ./gradlew testDebugUnitTest`
Expected: 编译失败(localIp/fakeLocalIp/probeDevice/isPrivateLanIp 未定义)

- [ ] **Step 3: 实现**

DevicesRepo.kt——接口加(注释块后):

```kotlin
    /**
     * 本机主网卡 IP(无网络时 null)
     */
    suspend fun localIp(): String?

    /**
     * 手动探测指定地址(仅 IP 补默认端口 47600)
     */
    suspend fun probeAddr(addr: String)
```

FfiDevicesRepo 加:

```kotlin
    override suspend fun localIp(): String? =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.localIp() }

    override suspend fun probeAddr(addr: String) =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { app.probeAddr(addr) }
```

FakeDevicesRepo 加(字段区):

```kotlin
    var fakeLocalIp: String? = "192.168.1.105"
    var probeError: Exception? = null
    var lastProbedAddr: String? = null
        private set
```

(方法区):

```kotlin
    override suspend fun localIp(): String? = fakeLocalIp

    override suspend fun probeAddr(addr: String) {
        lastProbedAddr = addr
        probeError?.let { throw it }
    }
```

注意:`lastProbedAddr` 需去掉 `private set` 若测试在同包访问——测试与 Fake 同模块不同包,保留 `var lastProbedAddr: String? = null` 公开即可(不 private set)。

DevicesUiModel.kt 加:

```kotlin
/**
 * 手动探测会话状态(null=无会话)
 */
data class ManualProbeUiState(
    val state: ProbeState,
    val target: String,
    val message: String = ""
)

enum class ProbeState {
    PROBING,
    FOUND,
    NOT_FOUND,
    ERROR
}

/**
 * IPv4 私网/回环判断(192.168.*/10.*/172.16-31.*/127.*);
 * 非私网(如蜂窝 CGNAT)时 UI 附"可能无法直连"提示
 */
fun isPrivateLanIp(ip: String): Boolean {
    val parts = ip.split('.')
    if (parts.size != 4) return false
    val a = parts[0].toIntOrNull() ?: return false
    val b = parts[1].toIntOrNull() ?: return false
    return when {
        a == 10 || a == 127 -> true
        a == 192 && b == 168 -> true
        a == 172 && b in 16..31 -> true
        else -> false
    }
}
```

DevicesUiState 加字段:

```kotlin
    val localIp: String? = null,
    val deviceName: String = "",
    val manualProbe: ManualProbeUiState? = null,
```

DevicesViewModel.kt:

init 块加 `loadLocalIp()`;`loadSettings` 的 update 里补 `deviceName = settings.deviceName`。

新增(`startTimeoutWatcher` 同款持 Job 模式):

```kotlin
    private var probeJob: Job? = null

    private fun loadLocalIp() {
        viewModelScope.launch {
            try {
                val ip = repo.localIp()
                _uiState.update { it.copy(localIp = ip ?: "") }
            } catch (e: Exception) {
                _uiState.update { it.copy(localIp = "") }
            }
        }
    }

    fun probeDevice(addr: String) {
        probeJob?.cancel()
        _uiState.update {
            it.copy(manualProbe = ManualProbeUiState(state = ProbeState.PROBING, target = addr))
        }
        probeJob = viewModelScope.launch {
            try {
                repo.probeAddr(addr)
            } catch (e: Exception) {
                _uiState.update {
                    it.copy(manualProbe = ManualProbeUiState(
                        state = ProbeState.ERROR, target = addr,
                        message = "探测发送失败: ${e.message}"
                    ))
                }
                return@launch
            }
            delay(5000)
            val targetIp = addr.substringBefore(':')
            val found = try {
                repo.devices().any { it.addr.substringBefore(':') == targetIp }
            } catch (e: Exception) {
                _uiState.update {
                    it.copy(manualProbe = ManualProbeUiState(
                        state = ProbeState.ERROR, target = addr,
                        message = "设备查询失败: ${e.message}"
                    ))
                }
                return@launch
            }
            if (found) {
                _uiState.update {
                    it.copy(manualProbe = ManualProbeUiState(
                        state = ProbeState.FOUND, target = addr, message = "已发现设备"
                    ))
                }
                loadDevices()
            } else {
                _uiState.update {
                    it.copy(manualProbe = ManualProbeUiState(
                        state = ProbeState.NOT_FOUND, target = addr,
                        message = "未发现设备:对方可能不在线、已开启隐身,或本机隐身中暂停了探测"
                    ))
                }
            }
        }
    }

    fun clearManualProbe() {
        probeJob?.cancel()
        _uiState.update { it.copy(manualProbe = null) }
    }
```

(import 需补 `kotlinx.coroutines.delay`;localIp 空串=获取失败,null=未加载,UI 按此区分)

- [ ] **Step 4: 跑测试转绿 + 全量回归**

Run: `cd android && ./gradlew testDebugUnitTest`
Expected: 新增 7 测试全过,既有 74+ 用例不回归

- [ ] **Step 5: Commit**

```bash
git add android/app/src/main/java/com/localtrans/app/data/DevicesRepo.kt android/app/src/main/java/com/localtrans/app/ui/devices/ android/app/src/test/
git commit -m "feat(android): 设备页手动探测状态机与数据层(localIp/probeAddr)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 4: 设备页 UI(本机信息卡 + 手动添加弹窗)

**Files:**
- Modify: `android/app/src/main/java/com/localtrans/app/ui/devices/DevicesScreen.kt`

**Interfaces:**
- Consumes: Task 3 的 `uiState.localIp/deviceName/manualProbe`、`viewModel.probeDevice/clearManualProbe`、`isPrivateLanIp`

- [ ] **Step 1: 实现本机信息卡**(MyFingerprintSection 之后、设备列表之前)

```kotlin
@Composable
fun LocalInfoCard(
    deviceName: String,
    localIp: String?,
    onAddDevice: () -> Unit
) {
    val clipboard = LocalClipboardManager.current
    val context = LocalContext.current
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp)
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    deviceName.ifEmpty { "本机" },
                    style = MaterialTheme.typography.titleMedium
                )
                val ipText = when (localIp) {
                    null -> "IP 获取中…"
                    "" -> "无法获取 IP(检查网络)"
                    else -> "IP $localIp"
                }
                Text(ipText, style = MaterialTheme.typography.bodySmall)
                if (localIp != null && localIp.isNotEmpty() && !isPrivateLanIp(localIp)) {
                    Text(
                        "移动网络下 IP 可能无法直连",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                }
            }
            IconButton(
                onClick = {
                    if (!localIp.isNullOrEmpty()) {
                        clipboard.setText(AnnotatedString(localIp))
                        Toast.makeText(context, "已复制 IP,发给对方手动添加即可", Toast.LENGTH_SHORT).show()
                    }
                }
            ) { Icon(Icons.Default.ContentCopy, contentDescription = "复制本机 IP") }
            IconButton(onClick = onAddDevice) {
                Icon(Icons.Default.Add, contentDescription = "手动添加设备")
            }
        }
    }
}
```

- [ ] **Step 2: 实现手动添加弹窗**

```kotlin
@Composable
fun ManualAddDialog(
    probe: ManualProbeUiState?,
    onProbe: (String) -> Unit,
    onDismiss: () -> Unit
) {
    var text by remember { mutableStateOf("") }
    val valid = text.isNotBlank() && (text.contains(':') || text.count { it == '.' } == 3)
    val probing = probe?.state == ProbeState.PROBING
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("手动添加设备") },
        text = {
            Column {
                OutlinedTextField(
                    value = text,
                    onValueChange = { text = it },
                    label = { Text("IP 或 IP:端口") },
                    placeholder = { Text("例如: 192.168.1.100") },
                    singleLine = true,
                    enabled = !probing
                )
                Text(
                    "只填 IP 时使用默认发现端口 47600;对方需在线且未开启隐身才会出现",
                    style = MaterialTheme.typography.labelSmall
                )
                when (probe?.state) {
                    ProbeState.PROBING -> Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.padding(top = 8.dp)
                    ) {
                        CircularProgressIndicator(modifier = Modifier.size(16.dp), strokeWidth = 2.dp)
                        Text("  探测中…", style = MaterialTheme.typography.bodySmall)
                    }
                    ProbeState.FOUND -> Text(
                        probe.message, style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.primary
                    )
                    ProbeState.NOT_FOUND, ProbeState.ERROR -> Text(
                        probe.message, style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error
                    )
                    null -> {}
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { onProbe(text.trim()) }, enabled = valid && !probing) {
                Text("探测")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text("关闭") }
        }
    )
}
```

- [ ] **Step 3: 接线**(DevicesScreen Scaffold 内容区,MyFingerprintSection 调用后)

```kotlin
            var showManualAdd by remember { mutableStateOf(false) }

            MyFingerprintSection(
                fingerprint = uiState.myFingerprint,
                hidden = uiState.hidden,
                onHiddenChange = { viewModel.setHidden(it) }
            )

            LocalInfoCard(
                deviceName = uiState.deviceName,
                localIp = uiState.localIp,
                onAddDevice = { showManualAdd = true }
            )

            if (showManualAdd) {
                ManualAddDialog(
                    probe = uiState.manualProbe,
                    onProbe = { viewModel.probeDevice(it) },
                    onDismiss = {
                        showManualAdd = false
                        viewModel.clearManualProbe()
                    }
                )
            }
```

(import 需补:`androidx.compose.ui.platform.LocalClipboardManager`、`LocalContext`、`androidx.compose.ui.text.AnnotatedString`、`android.widget.Toast`、`androidx.compose.material.icons.filled.Add`/`ContentCopy`;若 MyFingerprintSection 现有签名不同以现状为准)

- [ ] **Step 4: 编译验证**

Run: `cd android && ./gradlew :app:compileDebugKotlin testDebugUnitTest`
Expected: BUILD SUCCESSFUL,测试不回归

- [ ] **Step 5: Commit**

```bash
git add android/app/src/main/java/com/localtrans/app/ui/devices/DevicesScreen.kt
git commit -m "feat(android): 设备页本机信息卡(复制 IP)与手动添加设备弹窗

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 5: v0.10.3 收尾(版本/APK/dist/tag)

**Files:**
- Modify: `android/app/build.gradle.kts`(versionCode/versionName)
- Modify: `CHANGELOG.md`
- Create: `dist/localtrans-android-v0.10.3/`(APK+zip)

**Interfaces:** 无代码接口;产物 versionCode=14/versionName="0.10.3"

- [ ] **Step 1: 版本三处**

`android/app/build.gradle.kts`:`versionCode = 13` → `14`;`versionName = "0.10.2"` → `"0.10.3"`。

CHANGELOG.md 顶部加:

```markdown
## [0.10.3] - 2026-08-26

### 新增
- **Android 本机 IP 复制与手动添加设备**:设备页新增本机信息卡(设备名+IP,一键复制发给对方);「+」打开手动添加弹窗(输 IP 或 IP:端口 → 探测 → 设备入列),探测结果含未发现原因提示(不在线/隐身);蜂窝网非私网 IP 附"可能无法直连"提示
- FFI 新增 `local_ip`/`probe_addr`(仅 Android+FFI 变更,PC/relay 零改动;协议无变更,relay 服务器无需更新)
```

- [ ] **Step 2: 全量测试 + Release APK**

Run: `cd android && ./gradlew testDebugUnitTest assembleRelease`
Expected: 测试全绿;APK 产出(用 aapt 验 versionCode=14/versionName=0.10.3;apksigner 验签,JAVA_HOME=jdk17 路径)

- [ ] **Step 3: dist 组装 + 隐私核查**

```bash
mkdir -p dist/localtrans-android-v0.10.3
cp android/app/build/outputs/apk/release/app-release.apk dist/localtrans-android-v0.10.3/localtrans-v0.10.3-android.apk
# usage.md 从上一版目录复制后按需微调;Compress-Archive 打 zip;核查 zip 内无 data/ 目录(identity.key/cert.der 绝不进产物)
```

- [ ] **Step 4: Commit + tag**

```bash
git add android/app/build.gradle.kts CHANGELOG.md
git commit -m "chore: v0.10.3 收尾——Android 复制 IP+手动添加设备

Co-Authored-By: Claude <noreply@anthropic.com>"
git tag v0.10.3
```

- [ ] **Step 5: 装机双机验证清单**(交用户)

①手机 WiFi 下复制 IP → PC「手动添加设备」粘贴探测 → 手机出现在 PC 列表;②PC 复制 IP → 手机「+」添加 → PC 出现在手机列表;③输错 IP → 5s 后"未发现"提示含隐身说明;④蜂窝网下本机卡显示"移动网络下 IP 可能无法直连";⑤本机隐身时探测 → 同样落"未发现"(门控不发包)。
