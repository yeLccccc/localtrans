/**
 * 产品级功能开关。
 *
 * RELAY_ENABLED 控制中继(跨公网)功能的客户端入口:
 *  - true(当前):设置页「中继(跨公网)」卡片、设备卡强制走中继入口正常开放;
 *  - false:整体隐藏上述入口(后端代码与既有配置不受影响)。
 */
export const RELAY_ENABLED = true
