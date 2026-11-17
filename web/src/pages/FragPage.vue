<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { invoke } from "../api/tauri";
import { showConfirm, showResult } from "../composables/useModal";
import { toast } from "../composables/useToast";
import { drives } from "../stores/app";
import { fragDrive, fragLoading, fragReport } from "../stores/frag";

const optimizing = ref(false);

const driveList = computed(() => (drives.value.length ? drives.value : ["C:\\"]));

const status = computed(() => {
  const report = fragReport.value;
  if (fragLoading.value) return `状态：正在分析 ${fragDrive.value} …`;
  if (!report) return "状态：准备分析…";
  if (report.ssd === true) {
    return `状态：${fragDrive.value.slice(0, 2)} 是固态硬盘，不需要整理碎片。「一键清理」只会让系统做 TRIM 优化`;
  }
  if (report.percent == null && report.count == null) {
    return "状态：没能读到碎片分析。下方格子是示意，不是磁盘簇的真实布局";
  }
  return `状态：${fragDrive.value.slice(0, 2)} 分析完成。分布图为示意，按碎片率上色`;
});

const stats = computed(() => {
  const report = fragReport.value;
  return [
    ["碎片数", report && report.count != null ? String(report.count) : "—"],
    ["碎片文件", report && report.space_gb != null ? report.space_gb.toFixed(2) + " GB" : "—"],
    ["碎片率", report && report.percent != null ? report.percent + "%" : "—"],
  ] as const;
});

const GRID_TOTAL = 11 * 32;

function hotCells(percent: number | null | undefined): Set<number> {
  const hot = new Set<number>();
  if (percent == null) return hot;
  const rate = Math.max(0, Math.min(100, Number(percent)));
  const count = Math.round((GRID_TOTAL * rate) / 100);
  if (count >= GRID_TOTAL) {
    for (let index = 0; index < GRID_TOTAL; index += 1) hot.add(index);
    return hot;
  }
  const step = GRID_TOTAL / Math.max(count, 1);
  for (let index = 0; index < count; index += 1) {
    hot.add(Math.min(GRID_TOTAL - 1, Math.floor(index * step)));
  }
  let cursor = 0;
  while (hot.size < count && cursor < GRID_TOTAL) {
    hot.add(cursor);
    cursor += 1;
  }
  return hot;
}

const hot = computed(() => hotCells(fragReport.value?.percent));

async function analyze() {
  fragLoading.value = true;
  try {
    fragReport.value = await invoke("frag_analyze", { drive: fragDrive.value });
  } catch (error) {
    fragReport.value = null;
    toast("分析失败：" + error);
  }
  fragLoading.value = false;
}

function selectDrive(drive: string) {
  if (fragDrive.value === drive) return;
  fragDrive.value = drive;
  fragReport.value = null;
  void analyze();
}

async function optimize() {
  const drive = fragDrive.value;
  const ssd = fragReport.value?.ssd === true;
  const lines = ssd
    ? [`${drive.slice(0, 2)} 是固态硬盘，将让系统执行 TRIM 优化，不做碎片整理。`]
    : [`将对 ${drive.slice(0, 2)} 执行碎片整理，可能需要较长时间，期间请不要关闭窗口。`];
  if (!(await showConfirm("确认优化磁盘", lines, ssd ? "开始优化" : "开始整理"))) return;
  optimizing.value = true;
  try {
    await invoke("frag_optimize", { drive });
    await showResult("优化完成", ["磁盘优化已结束。"]);
  } catch (error) {
    toast("优化失败：" + error);
  }
  optimizing.value = false;
  fragReport.value = null;
  await analyze();
}

onMounted(() => {
  if (!fragReport.value && !fragLoading.value) void analyze();
});
</script>

<template>
  <div class="page-head">
    <div class="headline">清理系统碎片，加速电脑告别卡顿</div>
    <button class="green-btn" :disabled="optimizing" @click="optimize">
      {{ optimizing ? "优化中…" : "一键清理" }}
    </button>
  </div>
  <div class="sub">{{ status }}</div>

  <div class="tabs">
    <button
      v-for="drive in driveList"
      :key="drive"
      class="tab"
      :class="{ active: fragDrive === drive }"
      @click="selectDrive(drive)"
    >
      {{ drive.slice(0, 1) }}盘
    </button>
  </div>

  <div class="frag-stats">
    <div v-for="[label, value] in stats" :key="label" class="frag-stat">
      <div class="frag-label">{{ label }}</div>
      <div class="frag-value">{{ value }}</div>
    </div>
  </div>

  <div class="frag-grid">
    <div
      v-for="index in GRID_TOTAL"
      :key="index"
      class="frag-cell"
      :class="{ hot: hot.has(index - 1) }"
    ></div>
  </div>
  <div class="frag-note">示意（按碎片率上色，不是磁盘簇的真实布局）</div>
</template>
