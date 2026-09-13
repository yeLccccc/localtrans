/**
 * LocalTrans 主入口
 */

import { createApp } from 'vue'
import { createPinia } from 'pinia'
import App from './App.vue'
import router from './router'
import { vClickOutside } from './directives/clickOutside'
import { installTauriMockIfBrowser } from './test-support/tauriMock'
import { createLogBridge } from './lib/logBridge'
import './styles/design.css'

// 浏览器预览时注入演示数据(真实 Tauri 环境不干预)
installTauriMockIfBrowser()

const app = createApp(App)
const pinia = createPinia()

app.use(pinia)
app.use(router)
// Task M1: 前端 console/错误日志桥(spec §7.1.7-2)。
// 仅真实 Tauri 环境激活——探测排除 tauriMock 注入的浏览器实例，
// 与上面 installTauriMockIfBrowser 的调用顺序无关；浏览器/vitest 完全 no-op
app.use(createLogBridge(router))
app.directive('click-outside', vClickOutside)

// Task M2: 测试通道桥（spec §7.1.4）。动态 import + 构建期常量门：
// 正式构建（VITE_TEST_API 未定义）整段连同子模块被 rollup 剪除，
// dist 产物不含 __testBridge 特征；浏览器 mock/vitest 同样不注册。
// Task M3: 附带 pinia（state 动作生成四 store 聚合快照）
if (import.meta.env.VITE_TEST_API === '1') {
  void import('./test-support/testBridge')
    .then((m) => m.setupTestBridge(router, pinia))
    .catch((e) => console.warn('testBridge 初始化失败', e))
}

app.mount('#app')
