/**
 * Vue Router 配置
 * 4个主页面：设备、传输、浏览、设置
 */

import { createRouter, createWebHistory } from 'vue-router'
import type { RouteRecordRaw } from 'vue-router'

const routes: RouteRecordRaw[] = [
  {
    path: '/',
    redirect: '/devices',
  },
  {
    path: '/devices',
    name: 'devices',
    component: () => import('../pages/Devices.vue'),
    meta: { title: '设备' },
  },
  {
    path: '/transfers',
    name: 'transfers',
    component: () => import('../pages/Transfers.vue'),
    meta: { title: '传输' },
  },
  {
    path: '/browse',
    name: 'browse',
    component: () => import('../pages/Browse.vue'),
    meta: { title: '浏览' },
  },
  {
    path: '/settings',
    name: 'settings',
    component: () => import('../pages/Settings.vue'),
    meta: { title: '设置' },
  },
]

const router = createRouter({
  history: createWebHistory(),
  routes,
})

export default router
