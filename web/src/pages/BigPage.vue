<script setup lang="ts">
import { computed, onMounted } from "vue";
import { invoke } from "../api/tauri";
import type { BigFile, DeleteReport } from "../api/types";
import { showConfirm } from "../composables/useModal";
import { cancelActiveScan, scanStatus, startScanStatus, stopScanStatus } from "../composables/useScanStatus";
import { toast } from "../composables/useToast";
import { BIG_KINDS, BIG_NAMES, type BigKind } from "../config/sidebar";
import { openFile, revealFile } from "../lib/files";
import { diskOf, fmtSize, fmtTime, kindOf } from "../lib/format";
import { BIG_PAGE, bigChecked, bigLoading, bigRows, bigShown, bigTab } from "../stores/big";

const totals = computed(() => {
  const result: Record<string, number> = { all: 0 };
  for (const row of bigRows.value) {
    result.all += row.size;
    const kind = kindOf(row.path);
    result[kind] = (result[kind] || 0) + row.size;
  }
  return result;
});

const totalSize = computed(() => bigRows.value.reduce((sum, row) => sum + row.size, 0));

const visible = computed(() =>
  bigTab.value === "all" ? bigRows.value : bigRows.value.filter((row) => kindOf(row.path) === bigTab.value),
);

const shown = computed(() => visible.value.slice(0, bigShown.value));

const chosen = computed(() => bigRows.value.filter((row) => bigChecked.has(row.path)));
const chosenSize = computed(() => chosen.value.reduce((sum, row) => sum + row.size, 0));

const statusText = computed(() => {
  if (!bigLoading.value) {
    return "按类型分档扫描（视频/临时文件 20MB、文档/图片/日志 2MB、压缩包/音频 5MB、安装包 100MB、其他 50MB），删除前会先预览";
  }
  const cancelled = scanStatus.value?.big?.cancelled ? " 正在停止" : "";
  return `正在扫描各磁盘的大文件…${cancelled}`;
});

function selectTab(kind: BigKind) {
  bigTab.value = kind;
  bigShown.value = BIG_PAGE;
}

function selectAll() {
  for (const row of visible.value) bigChecked.add(row.path);
}

function selectNone() {
  bigChecked.clear();
}

function showMore() {
  bigShown.value += BIG_PAGE;
}

function toggle(row: BigFile) {
  if (bigChecked.has(row.path)) bigChecked.delete(row.path);
  else bigChecked.add(row.path);
}

async function scan() {
  bigLoading.value = true;
  bigShown.value = BIG_PAGE;
  startScanStatus();
  try {
    bigRows.value = await invoke<BigFile[]>("scan_big");
  } catch (error) {
    bigRows.value = [];
    toast("扫描失败：" + error);
  }
  bigLoading.value = false;
  stopScanStatus();
}

async function removeSelected() {
  const rows = chosen.value;
  if (!rows.length) {
    toast("请先勾选要清理的大文件");
    return;
  }
  const total = rows.reduce((sum, row) => sum + row.size, 0);
  const lines = [
    `将把 ${rows.length} 个文件移到回收站，约 ${fmtSize(total)}。`,
    "移到回收站后可以从回收站恢复。",
  ];
  if (!(await showConfirm("确认清理大文件", lines, "移到回收站"))) return;
  try {
    const report = await invoke<DeleteReport>("delete_recycle", {
      paths: rows.map((row) => row.path),
    });
    toast(`移到回收站 ${report.deleted} 个，释放 ${fmtSize(report.freed)}。`);
    bigChecked.clear();
    for (const path of report.failed || []) bigChecked.add(path);
  } catch (error) {
    toast("清理失败：" + error);
  }
  await scan();
}

onMounted(() => {
  if (!bigRows.value.length && !bigLoading.value) void scan();
});
</script>

<template>
  <div class="page-head">
    <div class="headline">
      发现 <span class="num">{{ bigRows.length }}</span> 个大文件，共
      <span class="num">{{ fmtSize(totalSize) }}</span>
    </div>
    <button class="green-btn" :disabled="false" @click="removeSelected">一键清理</button>
    <button v-if="bigLoading" class="gray-btn" @click="cancelActiveScan('big')">取消扫描</button>
  </div>
  <div class="sub">{{ statusText }}</div>

  <div class="tabs">
    <button
      v-for="kind in BIG_KINDS"
      :key="kind"
      class="tab"
      :class="{ active: bigTab === kind }"
      @click="selectTab(kind)"
    >
      {{ BIG_NAMES[kind] }}
      <span v-if="kind !== 'all' && totals[kind]" class="tab-size">{{ fmtSize(totals[kind]) }}</span>
    </button>
  </div>

  <div class="smart-tools">
    <button class="smart-btn" title="勾选当前分类下的全部文件" @click="selectAll">全选当前列表</button>
    <button class="smart-btn" @click="selectNone">取消全选</button>
    <span class="muted">已勾选 {{ chosen.length }} 个，共 {{ fmtSize(chosenSize) }}</span>
  </div>

  <div class="table">
    <div class="thead" style="grid-template-columns: 28px 1fr 60px 90px 130px 105px">
      <span></span><span>文件名称</span><span>磁盘</span><span>文件大小</span><span>修改时间</span><span>操作</span>
    </div>

    <div v-if="!visible.length" class="empty">{{ bigLoading ? "扫描中…" : "没有找到大文件。" }}</div>

    <div
      v-for="row in shown"
      :key="row.path"
      v-memo="[bigChecked.has(row.path)]"
      class="row"
      style="grid-template-columns: 28px 1fr 60px 90px 130px 105px"
      title="双击在资源管理器中定位"
      @dblclick="revealFile(row.path)"
    >
      <div class="check" :class="{ on: bigChecked.has(row.path) }" @click="toggle(row)"></div>
      <div class="name" :title="row.path">{{ row.path }}</div>
      <div class="muted">{{ diskOf(row.path) }}</div>
      <div class="muted">{{ fmtSize(row.size) }}</div>
      <div class="muted">{{ fmtTime(row.mtime) }}</div>
      <div class="acts">
        <button class="btn-act" title="在资源管理器中定位" @click.stop="revealFile(row.path)">定位</button>
        <button class="btn-act" title="打开文件" @click.stop="openFile(row.path)">打开</button>
      </div>
    </div>

    <button
      v-if="visible.length > bigShown"
      class="smart-btn"
      @click="showMore"
    >
      再显示 {{ Math.min(visible.length - bigShown, BIG_PAGE) }} 个（还有 {{ visible.length - bigShown }} 个）
    </button>
  </div>
</template>
