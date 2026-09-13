package com.localtrans.app

/**
 * 产品级功能开关。
 *
 * RELAY_ENABLED 控制中继功能的客户端入口:
 *  - true(当前):设置页「中继设置」卡片、设备卡强制走中继入口正常开放;
 *  - false:整体隐藏上述入口(FFI 能力与既有配置不受影响)。
 */
object AppFlags {
    const val RELAY_ENABLED: Boolean = true
}
