package com.localtrans.app.ui.transfers

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import com.localtrans.app.util.Formatters

@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
fun OfferSheet(
    sheet: OfferSheetUi,
    onAccept: () -> Unit,
    onReject: () -> Unit,
    onDismiss: () -> Unit
) {
    Dialog(onDismissRequest = onDismiss) {
        Card(
            modifier = Modifier
                .fillMaxWidth()
                .padding(16.dp)
                // Dialog 是独立窗口,不在 AppNav 根 Scaffold 的语义子树内——
                // testTagsAsResourceId 需在弹窗内单独开启,e2e(ch.dump)才能按
                // resource-id 读到按钮 testid(与桌面 09193ad 的 offer-accept-btn 同表)
                .semantics { testTagsAsResourceId = true },
            elevation = CardDefaults.cardElevation(defaultElevation = 8.dp)
        ) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(24.dp),
                horizontalAlignment = Alignment.CenterHorizontally
            ) {
                // Icon
                Icon(
                    imageVector = Icons.Default.Download,
                    contentDescription = null,
                    modifier = Modifier.size(48.dp),
                    tint = MaterialTheme.colorScheme.primary
                )

                Spacer(modifier = Modifier.height(16.dp))

                // Title
                Text(
                    text = "收到传输请求",
                    style = MaterialTheme.typography.titleLarge
                )

                Spacer(modifier = Modifier.height(8.dp))

                // Peer name
                Text(
                    text = "来自:${sheet.peerName}",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant
                )

                Spacer(modifier = Modifier.height(16.dp))

                // File count and total size
                Text(
                    text = "共 ${sheet.fileCount} 个文件,合计 ${Formatters.formatFileSize(sheet.totalSize.toULong())}",
                    style = MaterialTheme.typography.bodyLarge,
                    textAlign = TextAlign.Center
                )

                Spacer(modifier = Modifier.height(16.dp))

                // Countdown
                val countdownColor = if (sheet.remainingSecs <= 10) {
                    MaterialTheme.colorScheme.error
                } else {
                    MaterialTheme.colorScheme.onSurface
                }

                Text(
                    text = "${sheet.remainingSecs} 秒后自动拒绝",
                    style = MaterialTheme.typography.titleMedium,
                    color = countdownColor
                )

                Spacer(modifier = Modifier.height(24.dp))

                // Buttons
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    OutlinedButton(
                        onClick = onReject,
                        modifier = Modifier
                            .weight(1f)
                            .testTag("offer-reject-btn")
                    ) {
                        Icon(Icons.Default.Close, null)
                        Spacer(modifier = Modifier.width(8.dp))
                        Text("拒绝")
                    }

                    Button(
                        onClick = onAccept,
                        modifier = Modifier
                            .weight(1f)
                            .testTag("offer-accept-btn")
                    ) {
                        Icon(Icons.Default.Check, null)
                        Spacer(modifier = Modifier.width(8.dp))
                        Text("接收")
                    }
                }
            }
        }
    }
}
