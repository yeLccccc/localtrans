package com.localtrans.app.ui.screens

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.localtrans.app.ui.devices.DevicesScreen

@Composable
fun DevicesScreen() {
    // Import the real implementation from ui.devices
    com.localtrans.app.ui.devices.DevicesScreen()
}

@Composable
fun DevicesScreen(onBrowseShares: (String) -> Unit) {
    // Nav-aware wrapper: passes the browse-shares callback down
    com.localtrans.app.ui.devices.DevicesScreen(
        onBrowseShares = onBrowseShares
    )
}

@Composable
fun FilesScreen() {
    // Import the real implementation from ui.files
    val filesViewModel: com.localtrans.app.ui.files.FilesViewModel = androidx.lifecycle.viewmodel.compose.viewModel(
        factory = com.localtrans.app.ui.files.FilesViewModelFactory(
            com.localtrans.app.LocalTransBridge
        )
    )

    // Devices 页"共享文件夹"入口:带着目标设备指纹落到 REMOTE 浏览
    androidx.compose.runtime.LaunchedEffect(Unit) {
        com.localtrans.app.LocalTransBridge.consumePendingBrowseDevice()?.let { fp ->
            filesViewModel.browseDeviceShare(fp)
        }
    }

    com.localtrans.app.ui.files.FilesScreen(
        filesViewModel = filesViewModel
    )
}

@Composable
fun TransfersScreen() {
    // Import the real implementation from ui.transfers
    com.localtrans.app.ui.transfers.TransfersScreen(
        viewModel = androidx.lifecycle.viewmodel.compose.viewModel(
            factory = com.localtrans.app.ui.transfers.TransfersViewModelFactory(
                com.localtrans.app.LocalTransBridge
            )
        )
    )
}

@Composable
fun SettingsScreen() {
    // Import the real implementation from ui.settings
    com.localtrans.app.ui.settings.SettingsScreen(
        viewModel = androidx.lifecycle.viewmodel.compose.viewModel(
            factory = com.localtrans.app.ui.settings.SettingsViewModelFactory(
                com.localtrans.app.LocalTransBridge
            )
        )
    )
}

@Composable
private fun PlaceholderScreen(title: String, subtitle: String) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(16.dp),
        contentAlignment = Alignment.Center
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center
        ) {
            Text(
                text = title,
                style = MaterialTheme.typography.headlineMedium,
                textAlign = TextAlign.Center
            )
            Spacer(modifier = Modifier.height(8.dp))
            Text(
                text = subtitle,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center
            )
        }
    }
}

