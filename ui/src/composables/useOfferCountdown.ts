import { ref, watch, onUnmounted, type Ref } from 'vue'

/** v0.5.0 接收确认倒计时（500ms 心跳；≤10s urgent；到 0 expired 停表） */
export function useOfferCountdown(deadlineEpochMs: Ref<number | null>) {
  const remainingMs = ref(0)
  const expired = ref(false)
  const urgent = ref(false)
  let timer: number | null = null

  function tick() {
    if (deadlineEpochMs.value == null) return
    const left = deadlineEpochMs.value - Date.now()
    remainingMs.value = Math.max(0, left)
    urgent.value = left > 0 && left <= 10_000
    if (left <= 0) {
      expired.value = true
      stop()
    }
  }

  function stop() {
    if (timer !== null) { window.clearInterval(timer); timer = null }
  }

  function reset(deadlineMs: number) {
    expired.value = false
    deadlineEpochMs.value = deadlineMs
    tick()
  }

  watch(deadlineEpochMs, (v) => {
    stop()
    if (v == null) return
    expired.value = false
    tick()
    timer = window.setInterval(tick, 500)
  }, { immediate: true })

  onUnmounted(stop)

  return { remainingMs, expired, urgent, reset }
}
