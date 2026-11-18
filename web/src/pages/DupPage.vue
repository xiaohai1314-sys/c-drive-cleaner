<script setup lang="ts">
import { computed, onMounted } from "vue";
import { invoke } from "../api/tauri";
import type { DeleteReport, DupGroup } from "../api/types";
import { showConfirm } from "../composables/useModal";
import { cancelActiveScan, scanStatus, startScanStatus, stopScanStatus } from "../composables/useScanStatus";
import { toast } from "../composables/useToast";
import { drives } from "../stores/app";
import { applyKeeperRule, pickKeeper } from "../lib/dup";
import { openFile, revealFile } from "../lib/files";
import { fmtSize, fmtTime } from "../lib/format";
import {
  dupCachedAt,
  dupChecked,
  dupDrives,
  dupGroups,
  dupLoading,
  dupScope,
  type DupScope,
} from "../stores/dup";

const SCOPES: Array<[DupScope, string]> = [
  ["C:\\", "C盘"],
  ["USER", "用户目录"],
  ["ALL", "全部磁盘"],
  ["PICK", "按盘选择"],
];

const reclaim = computed(() =>
  dupGroups.value.reduce((sum, group) => sum + group.size * Math.max(group.files.length - 1, 0), 0),
);
const dupFileCount = computed(() =>
  dupGroups.value.reduce((sum, group) => sum + group.files.length, 0),
);

const subText = computed(() => {
  if (dupLoading.value) {
    const status = scanStatus.value?.dup;
    const stage = status?.stage || "查找重复文件";
    const counts = status?.total ? ` ${status.done}/${status.total}` : "";
    const found = status?.found ? `，已发现 ${status.found} 组` : "";
    const stopping = status?.cancelled ? " 正在停止" : "";
    return `正在${stage}${counts}${found}…${stopping}`;
  }
  if (dupScope.value === "PICK" && !dupDrives.size) {
    return "请选择要扫描的磁盘，然后点「扫描选中的磁盘」。";
  }
  const multiDrive = dupScope.value === "ALL" || dupScope.value === "PICK";
  const scopeNote = multiDrive ? "（多盘扫描只比对 ≥2MB 的文件）" : "";
  const picked =
    dupScope.value === "PICK"
      ? `已选 ${[...dupDrives].map((drive) => drive.replace(/\\+$/, "")).join("、")}，`
      : "";
  const stamp = dupCachedAt.value ? `上次扫描 ${fmtTime(dupCachedAt.value)} · ` : "";
  return `${picked}${stamp}每组自动保留一份，已勾选 ${dupChecked.size} 个副本${scopeNote}`;
});

const shownGroups = computed(() => dupGroups.value.slice(0, 300));

const driveOptions = computed(() => (drives.value.length ? drives.value : ["C:\\"]));

function selectScope(scope: DupScope) {
  if (dupScope.value === scope) return;
  dupScope.value = scope;
  dupGroups.value = [];
  dupChecked.clear();
  if (scope !== "PICK") void scan();
}

function toggleDrive(drive: string) {
  if (dupDrives.has(drive)) dupDrives.delete(drive);
  else dupDrives.add(drive);
  dupGroups.value = [];
  dupChecked.clear();
  dupCachedAt.value = 0;
}

function startPickedScan() {
  if (!dupDrives.size) {
    toast("请先选择要扫描的磁盘");
    return;
  }
  dupGroups.value = [];
  dupChecked.clear();
  void scan();
}

function selectDefaults() {
  dupChecked.clear();
  for (const group of dupGroups.value) {
    const keeper = pickKeeper(group);
    for (const path of group.files) if (path !== keeper) dupChecked.add(path);
  }
}

function uncheckAll() {
  dupChecked.clear();
}

function applyRule(rule: "newest" | "shortest") {
  const next = applyKeeperRule(dupGroups.value, rule);
  dupChecked.clear();
  for (const path of next) dupChecked.add(path);
  toast(rule === "newest" ? "已智能保留各组修改时间最新的文件" : "已智能保留各组路径最短的文件");
}

function togglePath(path: string, keeper: string) {
  if (path === keeper) return;
  if (dupChecked.has(path)) dupChecked.delete(path);
  else dupChecked.add(path);
}

interface DupScanArgs {
  root: string | null;
  drives: string[] | null;
  minSize: number | null;
  fast: boolean;
  [key: string]: unknown;
}

/** 当前页签对应的扫描参数；PICK 但没选盘时返回 null。 */
async function scanArgs(): Promise<DupScanArgs | null> {
  let root: string | null = null;
  let pickedDrives: string[] | null = null;
  if (dupScope.value === "C:\\") {
    root = "C:\\";
  } else if (dupScope.value === "USER") {
    root = await invoke<string>("user_profile");
  } else if (dupScope.value === "PICK") {
    pickedDrives = [...dupDrives];
    if (!pickedDrives.length) return null;
  }
  const multiDrive = dupScope.value === "ALL" || dupScope.value === "PICK";
  return {
    root,
    drives: pickedDrives,
    minSize: multiDrive ? 2 * 1024 * 1024 : null,
    // 与毒霸一致：只采样头/中/尾各 256KB 判定分组，删除前再逐字节核对。
    fast: true,
  };
}

async function scan() {
  dupLoading.value = true;
  startScanStatus();
  try {
    const args = await scanArgs();
    if (!args) return;
    dupGroups.value = await invoke<DupGroup[]>("scan_dups", args);
    dupCachedAt.value = Math.floor(Date.now() / 1000);
    selectDefaults();
  } catch (error) {
    dupGroups.value = [];
    dupCachedAt.value = 0;
    toast("扫描失败：" + error);
  } finally {
    dupLoading.value = false;
    stopScanStatus();
  }
}

/** 打开页面时先显示上次扫描的结果，不触发整盘扫描。 */
async function loadCached(): Promise<boolean> {
  try {
    const args = await scanArgs();
    if (!args) return true;
    const cached = await invoke<{ groups: DupGroup[]; cached_at: number }>("dup_cached_result", args);
    if (dupLoading.value || dupGroups.value.length) return true;
    if (!cached.cached_at) return false;
    dupGroups.value = cached.groups;
    dupCachedAt.value = cached.cached_at;
    selectDefaults();
    return true;
  } catch {
    return false;
  }
}

async function removeSelected() {
  const items: Array<{ path: string; hash: string; size: number; mtime: number; keeper: string }> = [];
  for (const group of dupGroups.value) {
    const keeper = pickKeeper(group);
    group.files.forEach((path, index) => {
      if (path === keeper || !dupChecked.has(path)) return;
      items.push({ path, hash: group.hash, size: group.size, mtime: group.mtimes[index] || 0, keeper });
    });
  }
  if (!items.length) {
    toast("请先勾选要清理的重复文件");
    return;
  }
  const total = items.reduce((sum, item) => sum + item.size, 0);
  const lines = [
    `将把 ${items.length} 个副本移到回收站，约 ${fmtSize(total)}。`,
    "每组保留一份；删除前会重新核对内容，扫描后变过的文件会跳过。",
    "扫描阶段只采样了头/中/尾各 256KB，删除前会把每个副本和保留文件完整比对一遍，数据量大时需要几分钟。",
  ];
  if (!(await showConfirm("确认清理重复文件", lines, "移到回收站"))) return;
  try {
    const report = await invoke<DeleteReport>("delete_dups", { items });
    toast(`移到回收站 ${report.deleted} 个，释放 ${fmtSize(report.freed)}。`);
  } catch (error) {
    toast("清理失败：" + error);
  }
  await scan();
}

onMounted(async () => {
  if (dupGroups.value.length || dupLoading.value || dupScope.value === "PICK") return;
  if (!(await loadCached())) void scan();
});
</script>

<template>
  <div class="page-head">
    <div class="headline">
      <template v-if="dupLoading">正在扫描重复文件…</template>
      <template v-else>
        发现 <span class="num">{{ dupGroups.length }}</span> 组重复文件，共
        <span class="num">{{ dupFileCount }}</span> 个文件、
        <span class="num">{{ fmtSize(reclaim) }}</span>
      </template>
    </div>
    <button class="green-btn" @click="removeSelected">一键清理</button>
    <button v-if="dupLoading" class="gray-btn" @click="cancelActiveScan('dup')">取消扫描</button>
    <button v-else class="gray-btn" @click="scan">重新扫描</button>
  </div>
  <div class="sub">{{ subText }}</div>

  <div class="tabs">
    <button
      v-for="[scope, label] in SCOPES"
      :key="scope"
      class="tab"
      :class="{ active: dupScope === scope }"
      @click="selectScope(scope)"
    >
      {{ label }}
    </button>
  </div>

  <div v-if="dupScope === 'PICK'" class="smart-tools">
    <span class="muted">选择要扫描的磁盘：</span>
    <button
      v-for="drive in driveOptions"
      :key="drive"
      class="smart-btn"
      :class="{ primary: dupDrives.has(drive) }"
      @click="toggleDrive(drive)"
    >
      {{ drive.replace(/\\+$/, "") }}
    </button>
    <button class="smart-btn primary" @click="startPickedScan">扫描选中的磁盘</button>
  </div>

  <div class="smart-tools">
    <button class="smart-btn" @click="applyRule('newest')">✨ 保留最新文件</button>
    <button class="smart-btn" @click="applyRule('shortest')">✨ 保留路径最短</button>
    <button class="smart-btn" @click="selectDefaults">全选所有副本</button>
    <button class="smart-btn" @click="uncheckAll">取消全选</button>
  </div>

  <div v-if="!dupGroups.length" class="empty">
    {{
      dupLoading
        ? "扫描中…"
        : dupScope === "PICK" && !dupDrives.size
          ? "请先选择磁盘并开始扫描。"
          : "没有找到重复文件。"
    }}
  </div>

  <template v-else>
    <div v-for="group in shownGroups" :key="group.hash" class="group">
      <div class="group-head">
        <b>{{ fmtSize(group.size) }}</b>
        <span> · {{ group.files.length }} 份 · 可省 </span>
        <b>{{ fmtSize(group.size * (group.files.length - 1)) }}</b>
      </div>
      <div
        v-for="(path, index) in group.files"
        :key="path"
        v-memo="[dupChecked.has(path), pickKeeper(group)]"
        class="row"
        style="grid-template-columns: 28px 1fr 90px 130px 105px"
        title="双击在资源管理器中定位"
        @dblclick="revealFile(path)"
      >
        <div
          class="check"
          :class="{ on: dupChecked.has(path) && path !== pickKeeper(group) }"
          :style="path === pickKeeper(group) ? { opacity: 0.35 } : undefined"
          @click="togglePath(path, pickKeeper(group))"
        ></div>
        <div class="name" :title="path">
          {{ path }}
          <span v-if="path === pickKeeper(group)" class="badge">保留</span>
        </div>
        <div class="muted">{{ fmtSize(group.size) }}</div>
        <div class="muted">{{ fmtTime(group.mtimes[index]) }}</div>
        <div class="acts">
          <button class="btn-act" title="在资源管理器中定位" @click.stop="revealFile(path)">定位</button>
          <button class="btn-act" title="打开文件" @click.stop="openFile(path)">打开</button>
        </div>
      </div>
    </div>
    <div v-if="dupGroups.length > 300" class="empty">
      只显示了前 300 组，共 {{ dupGroups.length }} 组。
    </div>
  </template>
</template>
