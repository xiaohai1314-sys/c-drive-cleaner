<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { activeTask, cancelTask } from "../stores/tasks";
import { fmtDuration } from "../lib/format";

const hiddenId = ref<string | null>(null);
const cancelling = ref(false);
const now = ref(Date.now());
let ticker: number | undefined;

onMounted(() => {
  ticker = window.setInterval(() => (now.value = Date.now()), 1000);
});
onUnmounted(() => {
  if (ticker) window.clearInterval(ticker);
});

watch(
  () => activeTask.value?.id ?? null,
  (id) => {
    if (!id) hiddenId.value = null;
    cancelling.value = false;
  },
);

const task = computed(() => {
  const current = activeTask.value;
  if (!current || current.id === hiddenId.value) return null;
  return current;
});

const percent = computed(() => {
  const current = task.value;
  if (!current || current.total <= 0) return null;
  return Math.min(100, (current.done / current.total) * 100);
});

const elapsed = computed(() => (task.value ? now.value - task.value.started_ms : 0));

const countText = computed(() => {
  const current = task.value;
  if (!current) return "";
  if (current.total > 0) return `${current.done}/${current.total} ${current.unit || ""}`;
  return current.done > 0 ? `${current.done} ${current.unit || ""}` : "";
});

const etaText = computed(() => {
  const current = task.value;
  const value = percent.value;
  if (!current || value === null || current.done <= 0 || current.done >= current.total) return "";
  const remain = (elapsed.value * (current.total - current.done)) / current.done;
  return ` · 剩余约 ${fmtDuration(remain)}`;
});

async function cancel() {
  const current = task.value;
  if (!current) return;
  cancelling.value = true;
  await cancelTask(current.id);
}
</script>

<template>
  <Teleport to="body">
    <div v-if="task" class="task-dock">
      <div class="task-head">
        <span class="task-title">{{ task.title || task.id }}</span>
        <button class="task-close" title="收起" @click="hiddenId = task.id">×</button>
      </div>
      <div class="task-stage">{{ task.cancelling ? "正在停止…" : (task.stage || "处理中") }}</div>
      <div class="task-bar" :class="{ indeterminate: percent === null }">
        <div class="task-fill" :style="{ width: percent === null ? '' : percent.toFixed(1) + '%' }"></div>
      </div>
      <div class="task-meta">
        <span class="task-count">{{ countText }}</span>
        <span class="task-time">已用 {{ fmtDuration(elapsed) }}{{ etaText }}</span>
      </div>
      <button
        class="task-cancel"
        :class="{ hidden: !task.cancellable }"
        :disabled="cancelling || task.cancelling"
        @click="cancel"
      >
        {{ cancelling || task.cancelling ? "正在停止…" : "取消" }}
      </button>
    </div>
  </Teleport>
</template>
