package com.localtrans.app.ui.devices

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import android.widget.Toast
import com.localtrans.app.AppFlags
import com.localtrans.app.ui.pairing.PairingDialog
import com.localtrans.app.LocalTransBridge
import uniffi.localtrans_ffi.ChannelDto

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DevicesScreen(
    viewModel: DevicesViewModel = viewModel(
        factory = DevicesViewModelFactory(LocalTransBridge)
    ),
    onBrowseShares: (String) -> Unit = {}
) {
    val uiState by viewModel.uiState.collectAsState()
    val devices by viewModel.devices.collectAsState()
    // M3c T2:通道表原始记录 + 面板目标设备(点击卡片通道标签弹出)
    val channels by viewModel.channels.collectAsState()
    val probingPeer by viewModel.probingPeer.collectAsState()
    var channelPanelFor by remember { mutableStateOf<DeviceUi?>(null) }

    // Show pairing dialog when needed
    uiState.pairing?.let { pairingState ->
        PairingDialog(
            pairingState = pairingState,
            onConsentResponse = { fp, accept -> viewModel.respondConsent(fp, accept) },
            onCodeSubmit = { fp, code -> viewModel.submitPairingCode(fp, code) },
            onCancelWait = { fp -> viewModel.cancelWait(fp) },
            onDismiss = { viewModel.clearPairing() }
        )
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("设备") },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.primaryContainer,
                    titleContentColor = MaterialTheme.colorScheme.onPrimaryContainer
                )
            )
        }
    ) { paddingValues ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(paddingValues)
        ) {
            var showManualAdd by remember { mutableStateOf(false) }

            // Error snackbar
            uiState.error?.let { msg ->
                Snackbar(
                    modifier = Modifier.padding(8.dp),
                    action = {
                        TextButton(onClick = { viewModel.clearError() }) {
                            Text("知道了")
                        }
                    }
                ) { Text(msg) }
            }

            // My fingerprint section
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

            // Device list
            if (devices.isEmpty()) {
                EmptyState()
            } else {
                DeviceList(
                    devices = devices,
                    onDeviceClick = { viewModel.connectDevice(it.fingerprint) },
                    onDisconnect = { viewModel.disconnect(it) },
                    onChannelClick = { channelPanelFor = it },
                    onSetForceRelay = { device, on -> viewModel.setForceRelay(device.fingerprint, on) },
                    onBrowseShares = onBrowseShares
                )
            }

            // M3c T2:通道面板(每地址明细 + 重新探测;沿用 Dialog + 独立开
            // testTagsAsResourceId 的惯例,供 e2e dump 按 resource-id 断言)
            channelPanelFor?.let { panelDevice ->
                ChannelPanelDialog(
                    device = panelDevice,
                    records = channels.filter { it.fingerprint == panelDevice.fingerprint },
                    probing = probingPeer == panelDevice.fingerprint,
                    onReprobe = { viewModel.probeNowPeer(panelDevice.fingerprint) },
                    onDismiss = { channelPanelFor = null }
                )
            }
        }
    }
}

@Composable
private fun MyFingerprintSection(
    fingerprint: String,
    hidden: Boolean,
    onHiddenChange: (Boolean) -> Unit
) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .padding(16.dp),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.secondaryContainer
        )
    ) {
        Column(
            modifier = Modifier.padding(16.dp)
        ) {
            Text(
                text = "我的设备指纹",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSecondaryContainer
            )

            Spacer(modifier = Modifier.height(4.dp))

            Text(
                text = if (fingerprint.length >= 8) fingerprint.take(8) else "...",
                style = MaterialTheme.typography.titleLarge,
                color = MaterialTheme.colorScheme.onSecondaryContainer
            )

            Spacer(modifier = Modifier.height(12.dp))

            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically
            ) {
                Text(
                    text = "隐身模式",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSecondaryContainer
                )

                Switch(
                    // M7:与桌面 devices-hidden-toggle 同名
                    modifier = Modifier.testTag("devices-hidden-toggle"),
                    checked = hidden,
                    onCheckedChange = onHiddenChange
                )
            }
        }
    }
}

@Composable
private fun LocalInfoCard(
    deviceName: String,
    localIp: String?,
    onAddDevice: () -> Unit
) {
    val clipboard = LocalClipboardManager.current
    val context = LocalContext.current
    // A7 隐私修复:复制 IP 后 45s 自动清空剪贴板
    val copyScope = rememberCoroutineScope()
    // v0.11.0 低危批:连续复制时先取消上一个 45s 定时,避免旧定时把新复制的
    // IP 提前/意外清空
    var clearTimer by remember { androidx.compose.runtime.mutableStateOf<kotlinx.coroutines.Job?>(null) }
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
                // M7:复制本机 IP(桌面同语义 devices-local-ip-chip)
                modifier = Modifier.testTag("devices-local-ip-chip"),
                onClick = {
                    if (!localIp.isNullOrEmpty()) {
                        clipboard.setText(AnnotatedString(localIp))
                        Toast.makeText(context, "已复制 IP,发给对方手动添加即可(45 秒后自动清空)", Toast.LENGTH_SHORT).show()
                        // A7:45s 后清空剪贴板,防止敏感 IP 长期驻留
                        clearTimer?.cancel()
                        clearTimer = copyScope.launch {
                            delay(45_000)
                            clipboard.setText(AnnotatedString(""))
                        }
                    }
                }
            ) { Icon(Icons.Default.ContentCopy, contentDescription = "复制本机 IP") }
            IconButton(
                // M7:打开手动添加对话框(桌面 devices-manual-add-open-btn)
                modifier = Modifier.testTag("devices-manual-add-open-btn"),
                onClick = onAddDevice
            ) {
                Icon(Icons.Default.Add, contentDescription = "手动添加设备")
            }
        }
    }
}

@Composable
private fun ManualAddDialog(
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
                    // M7:桌面 devices-manual-addr-input
                    modifier = Modifier.testTag("devices-manual-addr-input"),
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
            TextButton(
                modifier = Modifier.testTag("devices-manual-submit-btn"),
                onClick = { onProbe(text.trim()) },
                enabled = valid && !probing
            ) {
                Text("探测")
            }
        },
        dismissButton = {
            TextButton(
                modifier = Modifier.testTag("devices-manual-cancel-btn"),
                onClick = onDismiss
            ) { Text("关闭") }
        }
    )
}

@Composable
private fun DeviceList(
    devices: List<DeviceUi>,
    onDeviceClick: (DeviceUi) -> Unit,
    onDisconnect: (String) -> Unit,
    onChannelClick: (DeviceUi) -> Unit = {},
    onSetForceRelay: (DeviceUi, Boolean) -> Unit = { _, _ -> },
    onBrowseShares: (String) -> Unit = {}
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp)
    ) {
        items(
            items = devices,
            key = { it.fingerprint }
        ) { device ->
            DeviceCard(
                device = device,
                onClick = { onDeviceClick(device) },
                onDisconnect = { onDisconnect(device.fingerprint) },
                onChannelClick = { onChannelClick(device) },
                onSetForceRelay = { on -> onSetForceRelay(device, on) },
                onBrowseShares = { onBrowseShares(device.fingerprint) }
            )
        }
    }
}

@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
private fun DeviceCard(
    device: DeviceUi,
    onClick: () -> Unit,
    onDisconnect: () -> Unit,
    onChannelClick: () -> Unit = {},
    onSetForceRelay: (Boolean) -> Unit = {},
    onBrowseShares: () -> Unit = {}
) {
    var expanded by remember { mutableStateOf(false) }
    // M3c T3:⋮ 菜单开关(强制走中继)
    var forceRelayMenu by remember { mutableStateOf(false) }

    Card(
        // M7:动态前缀 id,与桌面 device-card-{fingerprint} 同构
        modifier = Modifier
            .fillMaxWidth()
            .testTag("device-card-${device.fingerprint}"),
        onClick = { if (!device.connected) onClick() }
    ) {
        Column(
            modifier = Modifier.padding(16.dp)
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically
            ) {
                Column(
                    modifier = Modifier.weight(1f)
                ) {
                    Row(
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text(
                            text = device.name,
                            style = MaterialTheme.typography.titleMedium,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis
                        )

                        if (device.connected) {
                            Spacer(modifier = Modifier.width(8.dp))
                            Surface(
                                color = MaterialTheme.colorScheme.primary,
                                shape = MaterialTheme.shapes.small
                            ) {
                                Text(
                                    text = "已连接",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onPrimary,
                                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp)
                                )
                            }
                        }
                    }

                    Spacer(modifier = Modifier.height(4.dp))

                    Text(
                        text = "指纹: ${device.fingerprint.take(8)}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )

                    // M3c T1:通道标签行(与 PC DeviceCard 地址行同构)
                    // 「直连 · 2ms」(绿点)/「经中继 · 120ms」(琥珀点)/「未知」(灰点)
                    // M3c T2:整行可点 → 通道面板
                    Spacer(modifier = Modifier.height(4.dp))
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .clip(MaterialTheme.shapes.small)
                            .clickable { onChannelClick() }
                            .testTag("device-channel-label-${device.fingerprint}")
                    ) {
                        Box(
                            modifier = Modifier
                                .size(8.dp)
                                .clip(CircleShape)
                                .background(
                                    when (device.channel?.kind) {
                                        ChannelKind.DIRECT -> Color(0xFF16A34A)
                                        ChannelKind.RELAY -> Color(0xFFF09A0A)
                                        null -> Color(0xFF9CA3AF)
                                    }
                                )
                        )
                        Spacer(modifier = Modifier.width(6.dp))
                        Text(
                            text = channelLabelText(device.channel),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.testTag("device-channel-label")
                        )
                    }
                }

                // Status indicators
                Row(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    if (device.viaRelay) {
                        Surface(
                            color = MaterialTheme.colorScheme.tertiaryContainer,
                            shape = MaterialTheme.shapes.small
                        ) {
                            Text(
                                text = "中继",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onTertiaryContainer,
                                modifier = Modifier.padding(horizontal = 6.dp, vertical = 2.dp)
                            )
                        }
                    }

                    // M3c T3:强制走中继角标(与 ⋮ 菜单勾选态同源)。发布态隐藏
                    if (AppFlags.RELAY_ENABLED && device.forceRelay) {
                        Surface(
                            color = Color(0xFFFFF7E6),
                            shape = MaterialTheme.shapes.small
                        ) {
                            Text(
                                text = "强制中继",
                                style = MaterialTheme.typography.labelSmall,
                                color = Color(0xFFF09A0A),
                                modifier = Modifier
                                    .padding(horizontal = 6.dp, vertical = 2.dp)
                                    .testTag("device-force-relay-badge-${device.fingerprint}")
                            )
                        }
                    }

                    if (device.online && !device.connected) {
                        Icon(
                            imageVector = Icons.Default.Circle,
                            contentDescription = "在线",
                            tint = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.size(12.dp)
                        )
                    }

                    // M3c T3:⋮ 菜单(强制走中继勾选项;桌面 DeviceCard 同语义)。发布态隐藏
                    if (AppFlags.RELAY_ENABLED) {
                    Box {
                        IconButton(
                            onClick = { forceRelayMenu = true },
                            modifier = Modifier
                                .size(28.dp)
                                .testTag("device-menu-btn-${device.fingerprint}")
                        ) {
                            Icon(
                                imageVector = Icons.Default.MoreVert,
                                contentDescription = "更多操作",
                                modifier = Modifier.size(20.dp)
                            )
                        }
                        DropdownMenu(
                            expanded = forceRelayMenu,
                            onDismissRequest = { forceRelayMenu = false },
                            // 弹层是独立窗口:单独开启 testTagsAsResourceId(OfferSheet 惯例),
                            // e2e(ch.dump)按 resource-id 读菜单勾选项
                            modifier = Modifier.semantics { testTagsAsResourceId = true }
                        ) {
                            Row(
                                verticalAlignment = Alignment.CenterVertically,
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .clickable {
                                        onSetForceRelay(!device.forceRelay)
                                        forceRelayMenu = false
                                    }
                                    .padding(horizontal = 12.dp, vertical = 8.dp)
                                    .testTag("device-force-relay-toggle")
                            ) {
                                Checkbox(
                                    checked = device.forceRelay,
                                    onCheckedChange = {
                                        onSetForceRelay(it)
                                        forceRelayMenu = false
                                    }
                                )
                                Text(
                                    text = "强制走中继",
                                    style = MaterialTheme.typography.bodyMedium
                                )
                            }
                        }
                    }
                    }
                }
            }

            // Expandable menu
            if (device.connected) {
                Spacer(modifier = Modifier.height(12.dp))

                Divider()

                Spacer(modifier = Modifier.height(12.dp))

                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween
                ) {
                    OutlinedButton(
                        onClick = onBrowseShares
                    ) {
                        Icon(
                            imageVector = Icons.Default.Folder,
                            contentDescription = null,
                            modifier = Modifier.size(18.dp)
                        )
                        Spacer(modifier = Modifier.width(4.dp))
                        Text("共享文件夹")
                    }

                    Button(
                        onClick = { expanded = !expanded },
                        colors = ButtonDefaults.buttonColors(
                            containerColor = MaterialTheme.colorScheme.error
                        )
                    ) {
                        Icon(
                            imageVector = Icons.Default.Close,
                            contentDescription = null,
                            modifier = Modifier.size(18.dp)
                        )
                        Spacer(modifier = Modifier.width(4.dp))
                        Text("断开连接")
                    }
                }
            }
        }
    }
}

/**
 * M3c T2:通道面板(与 PC ChannelPanel 同构)——每地址一行:
 * addr / 路径 / RTT / 估速 / 近10次丢包 / ✓当前使用 / 最近探测时间,
 * 底部「重新探测」= 手动单对端快检(ffi probePeerNow,20s 超时兜底)。
 * 沿用 Dialog + 独立开启 testTagsAsResourceId 的惯例(OfferSheet 同款),
 * e2e(ch.dump)按 resource-id 断言 testid。
 */
@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
private fun ChannelPanelDialog(
    device: DeviceUi,
    records: List<ChannelDto>,
    probing: Boolean,
    onReprobe: () -> Unit,
    onDismiss: () -> Unit
) {
    val rows = remember(records) { channelRows(records, device.fingerprint) }

    Dialog(onDismissRequest = onDismiss) {
        Card(
            modifier = Modifier
                .fillMaxWidth()
                .padding(16.dp)
                .semantics { testTagsAsResourceId = true }
                .testTag("device-channel-panel"),
            elevation = CardDefaults.cardElevation(defaultElevation = 8.dp)
        ) {
            Column(modifier = Modifier.fillMaxWidth().padding(20.dp)) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Column {
                        Text(
                            text = "${device.name} · 通道",
                            style = MaterialTheme.typography.titleMedium
                        )
                        Text(
                            text = "共 ${rows.size} 条地址记录",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                    }
                    TextButton(onClick = onDismiss) { Text("关闭") }
                }

                Spacer(modifier = Modifier.height(8.dp))
                Divider()

                if (rows.isEmpty()) {
                    Text(
                        text = "暂无通道记录(设备未连接或探测未完成)",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(vertical = 16.dp)
                    )
                } else {
                    for (row in rows) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(vertical = 6.dp)
                                .testTag("channel-row-${row.addr}")
                        ) {
                            Box(
                                modifier = Modifier
                                    .size(8.dp)
                                    .clip(CircleShape)
                                    .background(if (row.viaRelay) Color(0xFFF09A0A) else Color(0xFF16A34A))
                            )
                            Spacer(modifier = Modifier.width(8.dp))
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    text = row.addr,
                                    style = MaterialTheme.typography.bodySmall
                                )
                                Text(
                                    text = buildString {
                                        append(if (row.viaRelay) "中继" else "直连")
                                        append(" · ")
                                        append(row.rttMs?.let { "${it}ms" } ?: "RTT —")
                                        append(" · ")
                                        append(formatEstBps(row.estBps))
                                        append(" · 丢包 ")
                                        append("${(row.lossRate * 100).toInt()}%")
                                    },
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant
                                )
                            }
                            if (row.current) {
                                Text(
                                    text = "✓ 当前",
                                    style = MaterialTheme.typography.labelMedium,
                                    color = MaterialTheme.colorScheme.primary
                                )
                                Spacer(modifier = Modifier.width(8.dp))
                            }
                            Text(
                                text = formatAge(row.ageSecs),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant
                            )
                        }
                    }
                }

                Spacer(modifier = Modifier.height(8.dp))
                Divider()

                OutlinedButton(
                    onClick = onReprobe,
                    enabled = rows.any { it.current } && !probing,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 12.dp)
                        .testTag("channel-reprobe-btn")
                ) {
                    Text(if (probing) "探测中…" else "重新探测")
                }
            }
        }
    }
}

@Composable
private fun EmptyState() {
    Box(
        modifier = Modifier.fillMaxSize(),
        contentAlignment = Alignment.Center
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center
        ) {
            Icon(
                imageVector = Icons.Default.DevicesOther,
                contentDescription = null,
                modifier = Modifier.size(56.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant
            )

            Spacer(modifier = Modifier.height(12.dp))

            Text(
                text = "等待发现设备…",
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant
            )

            Spacer(modifier = Modifier.height(4.dp))

            Text(
                text = "确保设备在同一网络且应用已启动",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant
            )
        }
    }
}
