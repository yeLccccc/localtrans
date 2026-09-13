/** @vitest-environment jsdom */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { ref } from 'vue'
import { useOfferCountdown } from '../composables/useOfferCountdown'

describe('useOfferCountdown', () => {
  beforeEach(() => { vi.useFakeTimers() })
  afterEach(() => { vi.useRealTimers() })

  it('counts down and flags urgent under 10s', () => {
    const deadline = ref(Date.now() + 12_000)
    const { remainingMs, urgent, expired } = useOfferCountdown(deadline)
    expect(remainingMs.value).toBe(12_000)
    vi.advanceTimersByTime(2500)
    expect(remainingMs.value).toBeLessThanOrEqual(9500)
    expect(urgent.value).toBe(true)
    expect(expired.value).toBe(false)
  })

  it('expires at zero and stops', () => {
    const deadline = ref(Date.now() + 1000)
    const { remainingMs, expired } = useOfferCountdown(deadline)
    vi.advanceTimersByTime(1500)
    expect(expired.value).toBe(true)
    expect(remainingMs.value).toBe(0)
  })

  it('reset re-arms with a new deadline', () => {
    const deadline = ref(Date.now() + 1000)
    const { remainingMs, reset } = useOfferCountdown(deadline)
    vi.advanceTimersByTime(800)
    reset(Date.now() + 10_000)
    expect(remainingMs.value).toBeGreaterThan(9000)
  })
})
