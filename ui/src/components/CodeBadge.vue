<template>
  <div class="code-badge">
    <div class="code-display">{{ formattedCode }}</div>
    <div class="code-label">{{ label }}</div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'

interface Props {
  code: string
  label?: string
}

const props = withDefaults(defineProps<Props>(), {
  label: '配对码'
})

// 格式化配对码为 6 位显示 (每2位一组)
const formattedCode = computed(() => {
  const code = props.code.padStart(6, '0').slice(0, 6)
  return `${code.slice(0, 2)} ${code.slice(2, 4)} ${code.slice(4, 6)}`
})
</script>

<style scoped>
.code-badge {
  display: inline-block;
  text-align: center;
}

.code-display {
  font-family: 'Courier New', Courier, monospace;
  font-size: 48px;
  font-weight: bold;
  letter-spacing: 8px;
  color: var(--gray-800);
  background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
  -webkit-background-clip: text;
  -webkit-text-fill-color: transparent;
  background-clip: text;
  padding: 20px 40px;
  border-radius: var(--radius-md);
  background-color: var(--gray-50);
  border: 2px solid var(--gray-200);
  box-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.1);
}

.code-label {
  margin-top: 12px;
  font-size: var(--text-base);
  color: var(--gray-500);
  font-weight: 500;
}
</style>
