<template>
  <!-- 内联 SVG 线性图标:统一 1.5px stroke,currentColor 跟随导航态 -->
  <svg
    v-if="name === 'devices'"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.5"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <!-- 显示器 + 底座 = 设备 -->
    <rect x="3" y="4" width="18" height="12" rx="2" />
    <path d="M8 20h8" />
    <path d="M12 16v4" />
  </svg>

  <svg
    v-else-if="name === 'transfers'"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.5"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <!-- 双向箭头 = 传输 -->
    <path d="M7 4v12" />
    <path d="M3 8l4-4 4 4" />
    <path d="M17 20V8" />
    <path d="M13 16l4 4 4-4" />
  </svg>

  <svg
    v-else-if="name === 'browse'"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.5"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <!-- 文件夹 = 浏览共享区 -->
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" />
  </svg>

  <svg
    v-else-if="name === 'settings'"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.5"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <!-- 齿轮 = 设置 -->
    <circle cx="12" cy="12" r="3" />
    <path d="M12 2v3M12 19v3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M2 12h3M19 12h3M4.9 19.1L7 17M17 7l2.1-2.1" />
  </svg>
</template>

<script setup lang="ts">
defineProps<{
  name: 'devices' | 'transfers' | 'browse' | 'settings'
}>()
</script>

<style scoped>
svg {
  width: 24px;
  height: 24px;
  display: block;
}
</style>
