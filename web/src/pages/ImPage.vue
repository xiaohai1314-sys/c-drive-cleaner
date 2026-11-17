<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { invoke } from "../api/tauri";
import type { DeleteReport, ImPreview, ImRecord, MoveAnalysis } from "../api/types";
import ImageTile from "../components/ImageTile.vue";
import { showConfirm, showPreview, showResult } from "../composables/useModal";
import { toast } from "../composables/useToast";
import { IM_KINDS, type ImKind } from "../config/sidebar";
import { openFile, revealFile } from "../lib/files";
import { fmtSize, fmtTime } from "../lib/format";
import { loadDisk } from "../stores/app";
import { scanCategories } from "../stores/clean";
import { imStores } from "../stores/im";

const props = defineProps<{ kind: ImKind }>();
const store = imStores[props.kind];

const cleaning = ref(false);
const moving = ref(false);
const analyzingMove = ref(false);

const title = computed(() => (props.kind === "wechat" ? "微信文件" : "QQ 文件"));
const tabs = computed(() => IM_KINDS[props.kind]);

const totals = computed(() => {
  const result: Record<string, { size: number; count: number }> = {};
  for (const file of store.files.value) {
    const box = result[file.kind] || (result[file.kind] = { size: 0, count: 0 });
    box.size += file.size;
    box.count += 1;
  }
  return result;
});

const visible = computed(() => store.files.value.filter((file) => file.kind === store.tab.value));

const chosen = computed(() => store.files.value.filter((file) => store.checked.has(file.path)));
const chosenSize = computed(() => chosen.value.reduce((sum, file) => sum + file.size, 0));

const groups = computed(() => {
  const map = new Map<string, ImRecord[]>();
  for (const file of visible.value.slice(0, 2000)) {
    const list = map.get(file.month) || [];
    list.push(file);
    map.set(file.month, list);
  }
  return [...map.entries()];
});

const isGallery = computed(() => store.tab.value === "image" || store.tab.value === "sns");

// AppData 里是客户端程序数据（缓存、图标），不算「聊天文件」，也不该搬家。
function chatFileOnC(file: ImRecord): boolean {
  return (
    file.path.length > 1 &&
    file.path[1] === ":" &&
    file.path[0].toLowerCase() === "c" &&
    !file.path.toLowerCase().includes("\\appdata\\")
  );
}

const cFiles = computed(() => store.files.value.filter(chatFileOnC));
const canMove = computed(() => {
  const info = store.move.value;
  return info ? info.exists && !info.is_junction && info.size > 0 : cFiles.value.length > 0;
});
const moveSize = computed(() =>
  store.move.value ? store.move.value.size : cFiles.value.reduce((sum, file) => sum + file.size, 0),
);

function selectTab(kind: string) {
  store.tab.value = kind;
}

function selectAll() {
  for (const file of visible.value) store.checked.add(file.path);
}

function selectNone() {
  store.checked.clear();
}

function toggle(path: string) {
  if (store.checked.has(path)) store.checked.delete(path);
  else store.checked.add(path);
}

async function scan() {
  store.loading.value = true;
  try {
    store.files.value = await invoke<ImRecord[]>("im_scan", { kind: props.kind });
  } catch (error) {
    store.files.value = [];
    toast("扫描失败：" + error);
  }
  store.loading.value = false;
  // 扫描后不预选任何文件，删什么由用户自己勾。
  store.checked.clear();
}

async function clean() {
  const files = chosen.value;
  if (!files.length) {
    toast("请先勾选要清理的文件");
    return;
  }
  const total = files.reduce((sum, file) => sum + file.size, 0);
  const kinds = [...new Set(files.map((file) => file.kind))];
  const lines = [
    `将把 ${files.length} 个文件移到回收站，约 ${fmtSize(total)}。`,
    "聊天数据库、配置和密码类文件不会被清理。",
  ];
  if (!(await showConfirm("确认清理", lines, "移到回收站"))) return;
  cleaning.value = true;
  try {
    const report = await invoke<DeleteReport>("im_clean", {
      kind: props.kind,
      kinds,
      paths: files.map((file) => file.path),
    });
    const result = [`移到回收站 ${report.deleted} 个，释放 ${fmtSize(report.freed)}。`];
    for (const reason of (report.reasons || []).slice(0, 5)) result.push(reason);
    await showResult("清理完成", result);
  } catch (error) {
    toast("清理失败：" + error);
  }
  cleaning.value = false;
  store.files.value = [];
  store.checked.clear();
  await scan();
}

async function previewLog(path: string) {
  try {
    await showPreview(path, await invoke<ImPreview>("im_preview", { path }));
  } catch (error) {
    toast("预览失败：" + error);
  }
}

async function startMove() {
  const name = props.kind === "wechat" ? "微信" : "QQ";
  let info = store.move.value;
  if (!info) {
    analyzingMove.value = true;
    try {
      info = await invoke<MoveAnalysis>("im_move_analyze", { kind: props.kind });
      store.move.value = info;
    } catch (error) {
      toast("分析数据位置失败：" + error);
      analyzingMove.value = false;
      return;
    }
    analyzingMove.value = false;
  }
  if (!info.exists || !info.size) {
    toast(`没有检测到 C 盘的${name}数据`);
    return;
  }
  if (info.is_junction) {
    toast(`${name}数据已经搬家过了，无需重复操作`);
    return;
  }
  if (!info.target_drives || !info.target_drives.length) {
    toast("未检测到其他可用硬盘分区（当前仅有C盘）");
    return;
  }
  const targetDrive = info.target_drives[0].letter;
  const driveLabel = targetDrive.replace(/\\+$/, "");
  const base = targetDrive.endsWith("\\") ? targetDrive : targetDrive + "\\";
  const folder = props.kind === "wechat" ? "WeChat_Files_Migrated" : "QQ_Files_Migrated";
  const targetDir = `${base}${folder}`;
  const lines = [
    `源数据目录：${info.source}`,
    `数据总体积：${fmtSize(info.size)}（${info.file_count} 个文件）`,
    `目标迁移位置：${targetDir}`,
    "无感搬家机制：系统会将数据安全转移至目标磁盘，并在原 C 盘路径建立原生 NTFS 目录联接（Junction Point）。",
    `搬家后：${name}正常读写完全不受影响，聊天记录与文件 100% 完整保留，原 C 盘空间全额腾出！`,
    `⚠️ 关键提示：开始前请确认已彻底退出 ${name} 客户端！`,
  ];
  if (!(await showConfirm(`确认将 ${name} 数据迁移至 ${driveLabel} 盘`, lines, "开始搬家"))) return;

  moving.value = true;
  toast("正在搬家，数据量较大请耐心等待…");
  try {
    const message = await invoke<string>("im_move_execute", {
      source: info.source,
      targetDir,
      kind: props.kind,
    });
    await showResult("搬家成功", [message]);
    store.move.value = await invoke<MoveAnalysis>("im_move_analyze", { kind: props.kind });
    await scanCategories();
    await loadDisk();
    await scan();
  } catch (error) {
    await showResult("搬家失败", [String(error)]);
  }
  moving.value = false;
}

onMounted(() => {
  if (!store.files.value.length && !store.loading.value) void scan();
});
</script>

<template>
  <div class="page-head">
    <div class="headline">{{ title }}</div>
    <button class="green-btn" :disabled="cleaning" @click="clean">
      {{ cleaning ? "清理中…" : "清理勾选" }}
    </button>
  </div>
  <div class="sub">
    {{ store.loading.value ? "正在扫描聊天文件…" : `已勾选 ${chosen.length} 个，共 ${fmtSize(chosenSize)}` }}
  </div>

  <div v-if="canMove" class="move-banner">
    <div>
      <div class="info-title">🚀 {{ title }}存储一键搬家 (彻底释放 C 盘)</div>
      <div class="info-desc">
        检测到 {{ title }}数据在 C 盘{{ moveSize ? `（约 ${fmtSize(moveSize)}）` : "" }}。可一键迁移到其他硬盘分区并建立无缝软链接，不丢聊天记录与文件！
      </div>
    </div>
    <button class="move-btn" :disabled="moving || analyzingMove" @click="startMove">
      {{ analyzingMove ? "正在分析…" : moving ? "搬家进行中…" : "一键搬家到其他磁盘" }}
    </button>
  </div>

  <div class="tabs">
    <button
      v-for="[kind, label] in tabs"
      :key="kind"
      class="tab"
      :class="{ active: store.tab.value === kind }"
      @click="selectTab(kind)"
    >
      {{ label }}
      <span v-if="totals[kind]" class="tab-size">{{ fmtSize(totals[kind].size) }}</span>
    </button>
  </div>

  <div v-if="!visible.length" class="empty">
    {{ store.loading.value ? "扫描中…" : "没有找到这类文件。" }}
  </div>
  <template v-else>
    <div class="smart-tools">
      <button class="smart-btn" @click="selectAll">全选当前分类</button>
      <button class="smart-btn" @click="selectNone">取消全选</button>
    </div>

    <div v-for="[month, items] in groups" :key="month" class="group">
      <div class="group-head">
        <b>{{ month }}</b>
        <span> · {{ items.length }} 个 · {{ fmtSize(items.reduce((sum, file) => sum + file.size, 0)) }}</span>
      </div>

      <div v-if="isGallery" class="im-grid">
        <ImageTile
          v-for="file in items"
          :key="file.path"
          :file="file"
          :checked="store.checked.has(file.path)"
          @toggle="toggle(file.path)"
        />
      </div>

      <div
        v-for="file in (isGallery ? [] : items)"
        :key="file.path"
        v-memo="[store.checked.has(file.path)]"
        class="row"
        :style="{ gridTemplateColumns: file.kind === 'log' ? '28px 1fr 90px 130px 132px' : '28px 1fr 90px 130px 105px' }"
        title="双击在资源管理器中定位"
        @dblclick="revealFile(file.path)"
      >
        <div class="check" :class="{ on: store.checked.has(file.path) }" @click="toggle(file.path)"></div>
        <div class="name" :title="file.path">{{ file.name }}</div>
        <div class="muted">{{ fmtSize(file.size) }}</div>
        <div class="muted">{{ fmtTime(file.mtime) }}</div>
        <div class="acts">
          <button v-if="file.kind === 'log'" class="btn-act" title="查看日志内容" @click.stop="previewLog(file.path)">查看</button>
          <button class="btn-act" title="在资源管理器中定位" @click.stop="revealFile(file.path)">定位</button>
          <button class="btn-act" title="打开文件" @click.stop="openFile(file.path)">打开</button>
        </div>
      </div>
    </div>

    <div v-if="visible.length > 2000" class="empty">
      只显示了前 2000 个，共 {{ visible.length }} 个。
    </div>
  </template>
</template>
