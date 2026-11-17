import { ref } from "vue";

export const toastText = ref("");
export const toastShown = ref(false);

let timer: number | undefined;

export function toast(text: string, duration = 2200) {
  toastText.value = text;
  toastShown.value = true;
  if (timer) window.clearTimeout(timer);
  timer = window.setTimeout(() => {
    toastShown.value = false;
  }, duration);
}
