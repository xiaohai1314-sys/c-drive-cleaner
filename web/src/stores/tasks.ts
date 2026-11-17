import { computed, ref } from "vue";
import { invoke } from "../api/tauri";
import type { TaskStatus } from "../api/types";

export const tasks = ref<TaskStatus[]>([]);

/** 有写入操作的优先，其次按开始时间，只显示一个进行中的任务。 */
export const activeTask = computed<TaskStatus | null>(() => {
  const active = tasks.value.filter((item) => item.active);
  if (!active.length) return null;
  return active.sort(
    (a, b) => (Number(b.mutating) - Number(a.mutating)) || (b.started_ms - a.started_ms),
  )[0];
});

let timer: number | undefined;
let polling = false;

async function poll() {
  let delay = 1000;
  try {
    const map = await invoke<Record<string, TaskStatus>>("task_status");
    tasks.value = Object.values(map || {});
    if (tasks.value.some((item) => item.active)) delay = 400;
  } catch (_) {
    // 浏览器预览时没有 Tauri，安静跳过。
  }
  timer = window.setTimeout(poll, delay);
}

export function startTaskPolling() {
  if (polling) return;
  polling = true;
  void poll();
}

export function stopTaskPolling() {
  polling = false;
  if (timer) window.clearTimeout(timer);
  timer = undefined;
}

export async function cancelTask(id: string) {
  try {
    await invoke("cancel_task", { id });
  } catch (_) {
    // 任务可能已经结束，忽略。
  }
}
