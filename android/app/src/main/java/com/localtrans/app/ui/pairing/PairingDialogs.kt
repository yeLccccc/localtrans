package com.localtrans.app.ui.pairing

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.window.Dialog
import com.localtrans.app.ui.devices.PairingState
import com.localtrans.app.ui.devices.PairingUiState

/**
 * Pairing flow dialogs - Three states:
 * 1. CONSENT_REQUESTED - Show agree/reject buttons
 * 2. CODE_SHOWN - Display pairing code in large text
 * 3. CODE_ENTRY - Input field for pairing code
 */
@Composable
fun PairingDialog(
    pairingState: PairingUiState?,
    onConsentResponse: (fingerprint: String, accept: Boolean) -> Unit,
    onCodeSubmit: (fingerprint: String, code: String) -> Unit,
    onCancelWait: (fingerprint: String) -> Unit,
    onDismiss: () -> Unit
) {
    if (pairingState == null) return

    when (pairingState.state) {
        PairingState.CONSENT_REQUESTED -> {
            ConsentDialog(
                fingerprint = pairingState.fingerprint,
                peerName = pairingState.peerName,
                onAccept = { onConsentResponse(pairingState.fingerprint, true) },
                onReject = { onConsentResponse(pairingState.fingerprint, false) },
                onDismiss = onDismiss
            )
        }
        PairingState.CODE_SHOWN -> {
            CodeShownDialog(
                code = pairingState.code,
                onCancel = { onCancelWait(pairingState.fingerprint) },
                onDismiss = onDismiss
            )
        }
        PairingState.CODE_ENTRY -> {
            CodeEntryDialog(
                fingerprint = pairingState.fingerprint,
                peerName = pairingState.peerName,
                onSubmit = { code -> onCodeSubmit(pairingState.fingerprint, code) },
                onDismiss = onDismiss
            )
        }
        PairingState.WAITING_CONSENT -> {
            WaitingConsentDialog(
                peerName = pairingState.peerName,
                onDismiss = onDismiss
            )
        }
        PairingState.SUCCESS -> {
            SuccessDialog(
                onDismiss = onDismiss
            )
        }
        PairingState.FAILED -> {
            FailedDialog(
                reason = pairingState.reason,
                onDismiss = onDismiss
            )
        }
    }
}

@Composable
private fun ConsentDialog(
    fingerprint: String,
    peerName: String,
    onAccept: () -> Unit,
    onReject: () -> Unit,
    onDismiss: () -> Unit
) {
    Dialog(onDismissRequest = onDismiss) {
        Surface(
            shape = MaterialTheme.shapes.large,
            tonalElevation = 6.dp
        ) {
            Column(
                modifier = Modifier
                    .padding(24.dp)
                    .fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally
            ) {
                Text(
                    text = "配对请求",
                    style = MaterialTheme.typography.headlineSmall,
                    color = MaterialTheme.colorScheme.primary
                )

                Spacer(modifier = Modifier.height(16.dp))

                Text(
                    text = "$peerName 请求与您配对",
                    style = MaterialTheme.typography.bodyLarge,
                    textAlign = TextAlign.Center
                )

                Spacer(modifier = Modifier.height(8.dp))

                Text(
                    text = "设备指纹: ${fingerprint.take(8)}",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center
                )

                Spacer(modifier = Modifier.height(24.dp))

                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    OutlinedButton(
                        // M7:与桌面共用命名表(旧风格冻结 id,跨端场景直接复用)
                        modifier = Modifier
                            .weight(1f)
                            .testTag("btn-deny"),
                        onClick = onReject
                    ) {
                        Text("拒绝")
                    }

                    Button(
                        modifier = Modifier
                            .weight(1f)
                            .testTag("btn-grant"),
                        onClick = onAccept
                    ) {
                        Text("同意")
                    }
                }
            }
        }
    }
}

@Composable
private fun CodeShownDialog(
    code: String,
    onCancel: () -> Unit,
    onDismiss: () -> Unit
) {
    Dialog(onDismissRequest = onDismiss) {
        Surface(
            shape = MaterialTheme.shapes.large,
            tonalElevation = 6.dp
        ) {
            Column(
                modifier = Modifier
                    .padding(24.dp)
                    .fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally
            ) {
                Text(
                    text = "配对码",
                    style = MaterialTheme.typography.headlineSmall,
                    color = MaterialTheme.colorScheme.primary
                )

                Spacer(modifier = Modifier.height(16.dp))

                // Display code in large text
                Text(
                    text = code,
                    style = MaterialTheme.typography.displayLarge,
                    color = MaterialTheme.colorScheme.primary,
                    textAlign = TextAlign.Center,
                    modifier = Modifier.padding(vertical = 16.dp)
                )

                Spacer(modifier = Modifier.height(16.dp))

                Text(
                    text = "请在对方设备上输入此配对码",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center
                )

                Spacer(modifier = Modifier.height(24.dp))

                Button(
                    onClick = onCancel,
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Text("取消等待")
                }
            }
        }
    }
}

@Composable
private fun CodeEntryDialog(
    fingerprint: String,
    peerName: String,
    onSubmit: (String) -> Unit,
    onDismiss: () -> Unit
) {
    var code by remember { mutableStateOf("") }
    var isError by remember { mutableStateOf(false) }

    Dialog(onDismissRequest = onDismiss) {
        Surface(
            shape = MaterialTheme.shapes.large,
            tonalElevation = 6.dp
        ) {
            Column(
                modifier = Modifier
                    .padding(24.dp)
                    .fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally
            ) {
                Text(
                    text = "输入配对码",
                    style = MaterialTheme.typography.headlineSmall,
                    color = MaterialTheme.colorScheme.primary
                )

                Spacer(modifier = Modifier.height(16.dp))

                Text(
                    text = "请输入 $peerName 显示的配对码",
                    style = MaterialTheme.typography.bodyLarge,
                    textAlign = TextAlign.Center
                )

                Spacer(modifier = Modifier.height(16.dp))

                // 6-digit code input
                OutlinedTextField(
                    // M7:与桌面共用命名表(code-input)
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag("code-input"),
                    value = code,
                    onValueChange = {
                        if (it.length <= 6) {
                            code = it
                            isError = false
                        }
                    },
                    label = { Text("配对码") },
                    isError = isError,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    singleLine = true,
                    placeholder = { Text("123456") }
                )

                if (isError) {
                    Spacer(modifier = Modifier.height(4.dp))
                    Text(
                        text = "请输入6位数字",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error
                    )
                }

                Spacer(modifier = Modifier.height(24.dp))

                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    OutlinedButton(
                        onClick = onDismiss,
                        modifier = Modifier.weight(1f)
                    ) {
                        Text("取消")
                    }

                    Button(
                        modifier = Modifier
                            .weight(1f)
                            .testTag("btn-submit"),
                        onClick = {
                            if (code.length == 6) {
                                onSubmit(code)
                            } else {
                                isError = true
                            }
                        }
                    ) {
                        Text("提交")
                    }
                }
            }
        }
    }
}

@Composable
private fun WaitingConsentDialog(
    peerName: String,
    onDismiss: () -> Unit
) {
    Dialog(onDismissRequest = onDismiss) {
        Surface(
            shape = MaterialTheme.shapes.large,
            tonalElevation = 6.dp
        ) {
            Column(
                modifier = Modifier
                    .padding(24.dp)
                    .fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally
            ) {
                CircularProgressIndicator()

                Spacer(modifier = Modifier.height(16.dp))

                Text(
                    text = "等待对方确认...",
                    style = MaterialTheme.typography.titleMedium,
                    textAlign = TextAlign.Center
                )

                Spacer(modifier = Modifier.height(8.dp))

                Text(
                    text = "正在等待 $peerName 接受配对请求",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center
                )
            }
        }
    }
}

@Composable
private fun SuccessDialog(
    onDismiss: () -> Unit
) {
    Dialog(onDismissRequest = onDismiss) {
        Surface(
            shape = MaterialTheme.shapes.large,
            tonalElevation = 6.dp
        ) {
            Column(
                modifier = Modifier
                    .padding(24.dp)
                    .fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally
            ) {
                // Success icon (green checkmark)
                Text(
                    text = "✓",
                    style = MaterialTheme.typography.displayLarge,
                    color = MaterialTheme.colorScheme.primary
                )

                Spacer(modifier = Modifier.height(16.dp))

                Text(
                    text = "配对成功",
                    style = MaterialTheme.typography.titleLarge,
                    color = MaterialTheme.colorScheme.primary
                )

                Spacer(modifier = Modifier.height(24.dp))

                Button(
                    onClick = onDismiss,
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Text("确定")
                }
            }
        }
    }
}

@Composable
private fun FailedDialog(
    reason: String,
    onDismiss: () -> Unit
) {
    Dialog(onDismissRequest = onDismiss) {
        Surface(
            shape = MaterialTheme.shapes.large,
            tonalElevation = 6.dp
        ) {
            Column(
                modifier = Modifier
                    .padding(24.dp)
                    .fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally
            ) {
                // Error icon
                Text(
                    text = "✕",
                    style = MaterialTheme.typography.displayLarge,
                    color = MaterialTheme.colorScheme.error
                )

                Spacer(modifier = Modifier.height(16.dp))

                Text(
                    text = "配对失败",
                    style = MaterialTheme.typography.titleLarge,
                    color = MaterialTheme.colorScheme.error
                )

                Spacer(modifier = Modifier.height(8.dp))

                Text(
                    text = reason,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center
                )

                Spacer(modifier = Modifier.height(24.dp))

                Button(
                    onClick = onDismiss,
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Text("确定")
                }
            }
        }
    }
}
