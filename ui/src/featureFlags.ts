/**
 * 产品级功能开关(发布口径)。
 *
 * RELAY_ENABLED=false(当前发布态):
 *  - 设置页隐藏「中继(跨公网)」卡片,不加载/不订阅中继状态;
 *  - 设备卡隐藏「强制走中继」角标与 ⋮ 菜单勾选项。
 * 后端中继代码、Tauri 命令与既有配置原样保留,置 true 即整体恢复。
 */
export const RELAY_ENABLED = false
