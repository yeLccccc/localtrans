package com.localtrans.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.OnBackPressedCallback

import com.localtrans.app.ui.nav.AppNav
import com.localtrans.app.ui.theme.LocalTransTheme
import com.localtrans.app.util.StoragePermissions

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // Initialize LocalTransBridge with application context
        LocalTransBridge.init(applicationContext)

        // Handle intent extras (e.g., from notification deep link)
        handleIntent(intent)

        // 首启请求存储/通知权限:文件浏览器直读 /storage/emulated/0(Rust FFI 无
        // MediaStore 通道),运行时权限缺失时 listLocal 只会拿到空列表
        val permissionLauncher = registerForActivityResult(
            ActivityResultContracts.RequestMultiplePermissions()
        ) { /* 结果无需处理:未授权时文件页显示引导,设置里可再开 */ }
        permissionLauncher.launch(StoragePermissions.requiredPermissions(android.os.Build.VERSION.SDK_INT))

        setContent {
            LocalTransTheme {
                AppNav()
            }
        }
    }

    override fun onNewIntent(intent: android.content.Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleIntent(intent)
    }

    private fun handleIntent(intent: android.content.Intent?) {
        intent?.let {
            val openTab = it.getStringExtra("open_tab")
            if (openTab == "transfers") {
                LocalTransBridge.setPendingTab("transfers")
            }
        }
    }

    override fun onStart() {
        super.onStart()
    }

    override fun onStop() {
        super.onStop()
    }
}
