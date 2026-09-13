package com.localtrans.app.ui.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import com.localtrans.app.AppFlags
import com.localtrans.app.BuildConfig
import com.localtrans.app.debug.DebugTestHooks

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    viewModel: SettingsViewModel,
    modifier: Modifier = Modifier
) {
    val uiState by viewModel.uiState.collectAsState()
    val form by viewModel.formState.collectAsState()

    val scrollState = rememberScrollState()
    val snackbarHostState = remember { SnackbarHostState() }
    val scope = rememberCoroutineScope()

    // 保存成功 → snackbar 反馈(2s 自动消失)
    LaunchedEffect(uiState.savedTick) {
        if (uiState.savedTick > 0) {
            snackbarHostState.showSnackbar("已保存")
        }
    }
    // 保存/加载失败 → snackbar 提示
    LaunchedEffect(uiState.error) {
        uiState.error?.let {
            snackbarHostState.showSnackbar(it)
            viewModel.clearError()
        }
    }

    val offerTimeoutInvalid = isTimeoutInvalid(form.offerTimeoutSecs)
    val consentTimeoutInvalid = isTimeoutInvalid(form.consentTimeoutSecs)

    Scaffold(
        snackbarHost = { SnackbarHost(hostState = snackbarHostState) },
        topBar = {
            TopAppBar(
                title = { Text("设置") }
            )
        }
    ) { padding ->
        Column(
            modifier = modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(scrollState)
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp)
        ) {
            // Device settings card
            SettingsCard(title = "设备设置") {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    // Device name
                    OutlinedTextField(
                        // M7:桌面 settings-device-name-input
                        modifier = Modifier
                            .fillMaxWidth()
                            .testTag("settings-device-name-input"),
                        value = form.deviceName,
                        onValueChange = { viewModel.updateDeviceName(it) },
                        label = { Text("设备名称") },
                        singleLine = true
                    )

                    // Hidden mode
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text("隐身模式")
                            Text(
                                text = "开启后其他设备将发现不到本机",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant
                            )
                        }
                        Switch(
                            checked = form.hidden,
                            onCheckedChange = { viewModel.updateHidden(it) }
                        )
                    }
                }
            }

            // Timeout settings card
            SettingsCard(title = "超时设置") {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text(
                        text = "范围: 15-600 秒",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )

                    // Offer timeout
                    OutlinedTextField(
                        value = form.offerTimeoutSecs,
                        onValueChange = { viewModel.updateOfferTimeout(it) },
                        label = { Text("推送超时 (秒)") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                        isError = offerTimeoutInvalid,
                        supportingText = if (offerTimeoutInvalid) {
                            { Text("请输入 15-600 之间的数值") }
                        } else null
                    )

                    // Consent timeout
                    OutlinedTextField(
                        value = form.consentTimeoutSecs,
                        onValueChange = { viewModel.updateConsentTimeout(it) },
                        label = { Text("确认超时 (秒)") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                        isError = consentTimeoutInvalid,
                        supportingText = if (consentTimeoutInvalid) {
                            { Text("请输入 15-600 之间的数值") }
                        } else null
                    )
                }
            }

            // Relay settings card(发布态 AppFlags.RELAY_ENABLED=false 隐藏;FFI 能力与既有配置保留)
            if (AppFlags.RELAY_ENABLED) {
            SettingsCard(title = "中继设置") {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    // Relay enabled
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text("启用中继")
                        Switch(
                            // M7:桌面 settings-relay-enabled-toggle
                            modifier = Modifier.testTag("settings-relay-enabled-toggle"),
                            checked = form.relayEnabled,
                            onCheckedChange = { viewModel.updateRelayEnabled(it) }
                        )
                    }

                    if (form.relayEnabled) {
                        // Relay address
                        OutlinedTextField(
                            // M7:桌面 settings-relay-server-input
                            modifier = Modifier
                                .fillMaxWidth()
                                .testTag("settings-relay-server-input"),
                            value = form.relayAddr,
                            onValueChange = { viewModel.updateRelayAddr(it) },
                            label = { Text("中继地址") },
                            placeholder = { Text("example.com:9999") },
                            singleLine = true
                        )

                        // Relay PSK
                        OutlinedTextField(
                            // M7:桌面 settings-relay-psk-input
                            modifier = Modifier
                                .fillMaxWidth()
                                .testTag("settings-relay-psk-input"),
                            value = form.relayPsk,
                            onValueChange = { viewModel.updateRelayPsk(it) },
                            label = { Text("预共享密钥") },
                            singleLine = true,
                            visualTransformation = PasswordVisualTransformation()
                        )

                        // 状态行(v0.9.0):拉模式,进入页面/保存后刷新
                        Text(
                            text = "状态: ${uiState.relayStatusText}",
                            style = MaterialTheme.typography.bodySmall,
                            color = when {
                                uiState.relayStatusText.startsWith("已连接") -> MaterialTheme.colorScheme.primary
                                uiState.relayStatusText.startsWith("配置错误") -> MaterialTheme.colorScheme.error
                                else -> MaterialTheme.colorScheme.onSurfaceVariant
                            }
                        )
                    }
                }
            }
            }

            // Save button
            Button(
                // M7:settings-save-btn(桌面为分卡保存,此处整页一个保存按钮)
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag("settings-save-btn"),
                onClick = { viewModel.saveSettings() },
                enabled = !uiState.isSaving
            ) {
                if (uiState.isSaving) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(20.dp),
                        color = MaterialTheme.colorScheme.onPrimary
                    )
                    Spacer(modifier = Modifier.width(8.dp))
                    Text("保存中...")
                } else {
                    Icon(Icons.Default.Save, contentDescription = null)
                    Spacer(modifier = Modifier.width(8.dp))
                    Text("保存设置")
                }
            }

            // M7 测试钩子(debug 构建):e2e 编排器的配对自动化开关入口。
            // release 构建编译期剔除(BuildConfig.DEBUG 常量折叠),不进 UI。
            if (BuildConfig.DEBUG) {
                val context = LocalContext.current
                var autoConsent by remember {
                    mutableStateOf(DebugTestHooks.getAutoConsentPairing(context))
                }
                SettingsCard(title = "测试钩子 (仅 debug 构建)") {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text("自动同意配对")
                            Text(
                                text = "e2e 冒烟用;开启后配对请求直接同意,不再弹窗",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant
                            )
                        }
                        Switch(
                            modifier = Modifier.testTag("settings-test-auto-consent-toggle"),
                            checked = autoConsent,
                            onCheckedChange = {
                                DebugTestHooks.setAutoConsentPairing(context, it)
                                autoConsent = it
                            }
                        )
                    }
                }
            }
        }
    }
}

@Composable
fun SettingsCard(
    title: String,
    content: @Composable () -> Unit
) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        elevation = CardDefaults.cardElevation(defaultElevation = 2.dp)
    ) {
        Column(
            modifier = Modifier.padding(16.dp)
        ) {
            Text(
                text = title,
                style = MaterialTheme.typography.titleMedium
            )
            Spacer(modifier = Modifier.height(8.dp))
            content()
        }
    }
}
