package com.localtrans.app.ui.nav

import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.navigation.NavDestination.Companion.hierarchy
import androidx.navigation.NavGraphBuilder
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import com.localtrans.app.LocalTransBridge
import com.localtrans.app.bridge.EventRouter
import com.localtrans.app.ui.screens.*
import com.localtrans.app.ui.theme.LocalTransTheme
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.launch
import uniffi.localtrans_ffi.AppEvent

sealed class Screen(val route: String, val title: String, val icon: @Composable () -> Unit) {
    object Devices : Screen("devices", "设备", { Icon(Icons.Default.DevicesOther, contentDescription = null) })
    object Files : Screen("files", "文件", { Icon(Icons.Default.Folder, contentDescription = null) })
    object Transfers : Screen("transfers", "传输", { Icon(Icons.Default.SwapHoriz, contentDescription = null) })
    object Settings : Screen("settings", "设置", { Icon(Icons.Default.Settings, contentDescription = null) })
}

val screens = listOf(
    Screen.Devices,
    Screen.Files,
    Screen.Transfers,
    Screen.Settings
)

@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
fun AppNav() {
    val navController = rememberNavController()
    val navBackStackEntry by navController.currentBackStackEntryAsState()
    val currentDestination = navBackStackEntry?.destination

    // Handle deep link navigation from notification.
    // P3-T1:collect pendingTab StateFlow——冷启动首 collect 消费已有值;
    // 热启动 MainActivity.onNewIntent 重发后这里再次触发,前台点通知也能切页。
    // (原实现仅 LaunchedEffect(Unit) 首组合消费一次,热启动无人消费)
    LaunchedEffect(Unit) {
        LocalTransBridge.pendingTab.collect { tab ->
            if (tab != null) {
                navController.navigate(tab) {
                    popUpTo(navController.graph.startDestinationId) { saveState = true }
                    launchSingleTop = true
                    restoreState = true
                }
                LocalTransBridge.clearPendingTab()
            }
        }
    }

    // Handle delete confirmation events (P0-2d)
    var deleteAsk by remember { mutableStateOf<uniffi.localtrans_ffi.AppEvent.DeleteRequested?>(null) }
    // Handle incoming push offers globally — 弹窗挂在根层,任何页面都能看到
    // (原来挂在 TransfersScreen 里,人不在这页时 60s 倒计时默默流干被自动拒绝)
    var offerAsk by remember { mutableStateOf<com.localtrans.app.ui.transfers.OfferSheetUi?>(null) }
    LaunchedEffect(Unit) {
        EventRouter.events.collect { event ->
            when (event) {
                is uniffi.localtrans_ffi.AppEvent.DeleteRequested -> deleteAsk = event
                is uniffi.localtrans_ffi.AppEvent.OfferRequested -> {
                    offerAsk = com.localtrans.app.ui.transfers.OfferSheetUi(
                        jobId = event.jobId.toLong(),
                        peerName = event.peerName,
                        fileCount = event.fileCount.toInt(),
                        totalSize = event.totalSize.toLong(),
                        deadlineEpochMs = event.deadlineEpochMs,
                        remainingSecs = ((event.deadlineEpochMs - System.currentTimeMillis()) / 1000)
                            .toInt().coerceAtLeast(0)
                    )
                }
                else -> {}
            }
        }
    }

    // Offer 倒计时 ticker:每秒刷新剩余;归零清弹窗(core 侧 watchdog 会自动拒绝)
    if (offerAsk != null) {
        LaunchedEffect(offerAsk?.jobId) {
            while (offerAsk != null) {
                kotlinx.coroutines.delay(1000)
                val sheet = offerAsk ?: break
                val remaining = ((sheet.deadlineEpochMs - System.currentTimeMillis()) / 1000)
                    .toInt().coerceAtLeast(0)
                if (remaining <= 0) offerAsk = null else offerAsk = sheet.copy(remainingSecs = remaining)
            }
        }
    }

    Scaffold(
        // M7:testTag 以 resource-id 暴露给 uiautomator(挂在根布局,子树全生效)
        modifier = Modifier.semantics { testTagsAsResourceId = true },
        bottomBar = {
            NavigationBar {
                screens.forEach { screen ->
                    val selected = currentDestination?.hierarchy?.any { it.route == screen.route } == true
                    NavigationBarItem(
                        // M7:命名与桌面 testid-naming.md 同表(nav-<tab>-link)
                        modifier = Modifier.testTag("nav-${screen.route}-link"),
                        icon = { screen.icon() },
                        label = { Text(screen.title) },
                        selected = selected,
                        onClick = {
                            navController.navigate(screen.route) {
                                popUpTo(navController.graph.startDestinationId) { saveState = true }
                                launchSingleTop = true
                                restoreState = true
                            }
                        }
                    )
                }
            }
        }
    ) { innerPadding ->
        NavHost(
            navController = navController,
            startDestination = Screen.Devices.route,
            modifier = Modifier.padding(innerPadding)
        ) {
            addScreens(navController)
        }
    }

    // Global offer sheet — 接收确认弹窗(任意页面置顶显示)
    offerAsk?.let { sheet ->
        com.localtrans.app.ui.transfers.OfferSheet(
            sheet = sheet,
            onAccept = {
                offerAsk = null
                respondOfferLocal(sheet.jobId.toULong(), true)
            },
            onReject = {
                offerAsk = null
                respondOfferLocal(sheet.jobId.toULong(), false)
            },
            onDismiss = { offerAsk = null }
        )
    }

    // Delete confirmation dialog (P0-2d)
    deleteAsk?.let { ask ->
        AlertDialog(
            onDismissRequest = { respondDeleteLocal(ask.askId, false).also { deleteAsk = null } },
            title = { Text("删除确认") },
            text = {
                Text(
                    "${ask.peerName} 请求删除共享文件夹中的「${ask.name}」" +
                        (if (ask.isDir) "(文件夹,共 ${ask.entryCount} 项)" else "")
                )
            },
            confirmButton = {
                TextButton(onClick = { respondDeleteLocal(ask.askId, true).also { deleteAsk = null } }) { Text("允许删除") }
            },
            dismissButton = {
                TextButton(onClick = { respondDeleteLocal(ask.askId, false).also { deleteAsk = null } }) { Text("拒绝") }
            }
        )
    }
}

fun NavGraphBuilder.addScreens(navController: NavHostController) {
    composable(Screen.Devices.route) {
        DevicesScreen(
            onBrowseShares = { fp ->
                LocalTransBridge.setPendingBrowseDevice(fp)
                navController.navigate(Screen.Files.route) {
                    popUpTo(navController.graph.startDestinationId) { saveState = true }
                    launchSingleTop = true
                    restoreState = true
                }
            }
        )
    }
    composable(Screen.Files.route) { FilesScreen() }
    composable(Screen.Transfers.route) { TransfersScreen() }
    composable(Screen.Settings.route) { SettingsScreen() }
}

/**
 * Helper function to respond to delete requests (P0-2d)
 * Called from delete confirmation dialog in AppNav
 */
private fun respondDeleteLocal(askId: kotlin.ULong, allow: Boolean) {
    // M-C9: FFI 直调是阻塞调用,移到 IO 线程避免主线程 ANR
    kotlinx.coroutines.CoroutineScope(kotlinx.coroutines.Dispatchers.IO).launch {
        try {
            LocalTransBridge.app.respondDelete(askId, allow)
        } catch (e: Exception) {
            // Bridge 未初始化时静默——fail-closed 由 core 兜底(超时自动拒绝)
        }
    }
}

/**
 * Helper function to respond to push offers.
 * Called from the global offer sheet in AppNav.
 */
private fun respondOfferLocal(jobId: kotlin.ULong, accept: kotlin.Boolean) {
    // M-C9: 同上,移到 IO 线程
    kotlinx.coroutines.CoroutineScope(kotlinx.coroutines.Dispatchers.IO).launch {
        try {
            LocalTransBridge.app.respondOffer(jobId, accept)
        } catch (e: Exception) {
            // Bridge 未初始化/任务已超时——core 侧 watchdog 兜底自动拒绝
        }
    }
}
