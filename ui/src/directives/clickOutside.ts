import type { Directive } from 'vue'

export const vClickOutside: Directive<HTMLElement, () => void> = {
  beforeMount(el, binding) {
    el._clickOutsideHandler = (ev: MouseEvent) => {
      if (!el.contains(ev.target as Node)) {
        binding.value()
      }
    }
    document.addEventListener('click', el._clickOutsideHandler, true)
  },
  unmounted(el) {
    document.removeEventListener('click', el._clickOutsideHandler!, true)
  },
}

declare global {
  interface HTMLElement {
    _clickOutsideHandler?: (ev: MouseEvent) => void
  }
}
