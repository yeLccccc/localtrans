<template>
  <div class="modal-backdrop" @click.self="resolve(false)">
    <div class="modal-card confirm-card" data-testid="confirm-dialog">
      <h3>{{ title }}</h3>
      <p class="confirm-message">{{ message }}</p>
      <div v-if="hint" class="confirm-hint">{{ hint }}</div>
      <div v-if="inputVisible" class="confirm-input-row">
        <input
          ref="inputEl"
          v-model="inputValue"
          class="confirm-input"
          :placeholder="inputPlaceholder"
          data-testid="confirm-input"
          @keyup.enter="resolve(true)"
        />
      </div>
      <div class="modal-actions">
        <button class="btn-secondary" data-testid="confirm-cancel" @click="resolve(false)">取消</button>
        <button class="btn-danger" data-testid="confirm-ok" @click="resolve(true)">{{ okText }}</button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, watch, computed } from 'vue'

const props = withDefaults(
  defineProps<{
    title: string
    message: string
    hint?: string
    okText?: string
    inputInitial?: string
    inputPlaceholder?: string
  }>(),
  { okText: '确定', inputInitial: '', inputPlaceholder: '' }
)

const inputVisible = computed(() => props.inputInitial !== undefined || props.inputPlaceholder !== '')

const inputValue = ref(props.inputInitial)
const inputEl = ref<HTMLInputElement | null>(null)

// 打开时聚焦并全选,方便直接改写默认值
watch(inputEl, (el) => {
  if (el) {
    el.focus()
    el.select()
  }
})

const emit = defineEmits<{
  (e: 'resolve', ok: boolean, input?: string): void
}>()

function resolve(ok: boolean) {
  emit('resolve', ok, inputValue.value)
}
</script>

<style scoped>
.modal-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(17, 24, 39, 0.45);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 9999;
}

.modal-card {
  background: white;
  padding: var(--space-6);
  border-radius: var(--radius-md);
  min-width: 320px;
  max-width: 540px;
  box-shadow: var(--shadow-md);
}

.modal-card h3 {
  margin: 0 0 var(--space-4) 0;
  color: var(--gray-800);
}

.confirm-message {
  margin: 0 0 var(--space-3) 0;
  color: var(--gray-700);
  word-break: break-all;
}

.confirm-hint {
  margin: 0 0 var(--space-3) 0;
  padding: var(--space-2) var(--space-3);
  background: var(--gray-50);
  border-radius: var(--radius-sm);
  font-size: var(--text-xs);
  color: var(--gray-500);
  line-height: 1.5;
}

.confirm-input-row {
  margin: 0 0 var(--space-3) 0;
}

.confirm-input {
  width: 100%;
  box-sizing: border-box;
  padding: 8px 12px;
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-sm);
  font-size: var(--text-base);
}

.confirm-input:focus {
  outline: none;
  border-color: var(--primary-500);
  box-shadow: 0 0 0 3px rgba(59, 130, 246, 0.1);
}

.modal-actions {
  display: flex;
  justify-content: flex-end;
  gap: var(--space-3);
  margin-top: var(--space-4);
}

.btn-secondary,
.btn-danger {
  padding: 8px 16px;
  border-radius: var(--radius-sm);
  font-size: var(--text-base);
  font-weight: 500;
  cursor: pointer;
  border: none;
  transition: all 0.2s;
}

.btn-secondary {
  background: var(--gray-500);
  color: white;
}

.btn-secondary:hover {
  background: var(--gray-600);
}

.btn-danger {
  background: var(--danger-500);
  color: white;
}

.btn-danger:hover {
  background: var(--danger-600);
}
</style>
