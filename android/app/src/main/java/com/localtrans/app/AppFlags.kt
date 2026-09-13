package com.localtrans.app

/**
 * 产品级功能开关(发布口径)。
 *
 * RELAY_ENABLED=false(当前发布态):
 *  - 设置页隐藏「中继设置」卡片,不拉取中继状态;
 *  - 设备卡隐藏「强制中继」角标与 ⋮ 菜单勾选项。
 * FFI/后端中继能力与既有配置原样保留,置 true 即整体恢复。
 */
object AppFlags {
    const val RELAY_ENABLED: Boolean = false
}
