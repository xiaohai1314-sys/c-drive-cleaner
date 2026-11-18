<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import AppIcon from "../components/AppIcon.vue";
import { invoke } from "../api/tauri";
import type { CategorySize } from "../api/types";
import { showConfirm, showResult } from "../composables/useModal";
import { toast } from "../composables/useToast";
import { colorFor, fmtSize } from "../lib/format";
import { iconForCategory } from "../lib/icons";
import { disk, loadDisk } from "../stores/app";
import { categories, scanCategories, scanningClean, selected } from "../stores/clean";

interface CleanReport {
  freed: number;
  deleted: number;
  pending?: number;
  skipped?: number;
  notes?: string[];
  errors?: string[];
}

const cleaning = ref(false);

const found = computed(() =>
  categories.value.reduce((sum, cat) => sum + Math.max(cat.size, 0), 0),
);
const chosenSize = computed(() =>
  categories.value
    .filter((cat) => selected.has(cat.key))
    .reduce((sum, cat) => sum + Math.max(cat.size, 0), 0),
);

const recommended = computed(() =>
  categories.value.filter(
    (cat) => cat.implemented !== false && !cat.requires_confirm && cat.tier !== "deep",
  ),
);
const risky = computed(() =>
  categories.value.filter((cat) => cat.requires_confirm || cat.tier === "deep"),
);

const diskRatio = computed(() => {
  const info = disk.value;
  return info && info.total ? info.used / info.total : 0;
});

function sectionSize(list: CategorySize[]): string {
  const picked = list
    .filter((cat) => selected.has(cat.key))
    .reduce((sum, cat) => sum + Math.max(cat.size, 0), 0);
  const all = list.reduce((sum, cat) => sum + Math.max(cat.size, 0), 0);
  return `已选${fmtSize(picked)}/共${fmtSize(all)}`;
}

function cardTitle(cat: CategorySize): string {
  if (cat.implemented === false) return "该清理项需要系统命令，当前版本暂未迁移。";
  return (cat.describe || "") + (cat.warning ? "\n\n注意：" + cat.warning : "");
}

function toggle(cat: CategorySize) {
  if (cat.implemented === false) {
    toast("该项目暂未支持清理");
    return;
  }
  if (selected.has(cat.key)) selected.delete(cat.key);
  else selected.add(cat.key);
}

async function cleanSelected() {
  const chosen = categories.value.filter((cat) => selected.has(cat.key));
  if (!chosen.length) {
    toast("请先勾选要清理的项目");
    return;
  }

  // 运行进程冲突检测：先只检测，真正关闭放在用户确认清理之后。
  let running: string[] = [];
  try {
    running = await invoke<string[]>("check_processes", {
      targets: ["wechat.exe", "qq.exe", "xwechat.exe", "chrome.exe", "msedge.exe", "firefox.exe"],
    });
  } catch (_) {
    running = [];
  }

  const total = chosen.reduce((sum, cat) => sum + Math.max(cat.size, 0), 0);
  const deep = chosen.filter((cat) => cat.requires_confirm);
  const lines = [
    `将清理 ${chosen.length} 项，预计释放 ${fmtSize(total)}。`,
    "清理会直接删除文件，不进回收站。",
  ];
  if (deep.length) {
    lines.push(`其中 ${deep.length} 项含需确认的数据：${deep.map((cat) => cat.name).join("、")}。`);
  }
  if (running.length) {
    lines.push(`检测到 ${running.join("、")} 正在运行，开始清理时会先关闭它们（请先保存未完成的工作）。`);
  }
  if (!(await showConfirm("确认清理", lines, "开始清理"))) return;

  for (const proc of running) {
    try {
      await invoke("kill_process", { name: proc });
    } catch (_) {
      // 进程可能已经退出。
    }
  }

  cleaning.value = true;
  try {
    const report = await invoke<CleanReport>("clean_categories", {
      keys: chosen.map((cat) => cat.key),
    });
    const result = [`释放 ${fmtSize(report.freed)}，删除 ${report.deleted} 个文件。`];
    if (report.pending) result.push(`有 ${report.pending} 个文件被占用，已安排在重启后删除。`);
    if (report.skipped) result.push(`跳过 ${report.skipped} 项（受保护、正在使用或规则不允许）。`);
    for (const note of report.notes || []) result.push(note);
    for (const error of (report.errors || []).slice(0, 5)) result.push(error);
    await showResult("清理完成", result);
  } catch (error) {
    toast("清理失败：" + error);
  }
  cleaning.value = false;
  await scanCategories();
  await loadDisk();
}

onMounted(() => {
  if (!categories.value.length && !scanningClean.value) void scanCategories();
});
</script>

<template>
  <div class="page-head">
    <div class="headline">
      发现 <span class="num">{{ fmtSize(found) }}</span> 可释放，已勾选
      <span class="num">{{ fmtSize(chosenSize) }}</span>
    </div>
    <button class="green-btn" :disabled="cleaning" @click="cleanSelected">
      {{ cleaning ? "清理中…" : "一键瘦身" }}
    </button>
  </div>
  <div class="sub">{{ scanningClean ? "正在统计各类缓存…" : `已选 ${selected.size} 项` }}</div>

  <div class="disk-row">
    <div class="disk-bar">
      <div class="disk-fill" :style="{ width: Math.max(3, Math.round(diskRatio * 100)) + '%' }"></div>
    </div>
    <span class="disk-text">
      {{ disk ? `C: 共${fmtSize(disk.total)}，已用${fmtSize(disk.used)}` : "正在读取磁盘信息…" }}
    </span>
  </div>

  <div v-for="section in [
    { title: '推荐清理项', note: '系统和软件的垃圾和无用文件，可放心清理', list: recommended },
    { title: '谨慎清理项', note: '含需确认的数据及不可恢复日志，勾选前请查看提示', list: risky },
  ]" :key="section.title" v-show="section.list.length" class="section">
    <div class="section-title">
      <span>{{ section.title }}</span>
      <span class="note">{{ section.note }}</span>
      <span class="sizes">{{ sectionSize(section.list) }}</span>
    </div>
    <div class="cards">
      <div
        v-for="cat in section.list"
        :key="cat.key"
        class="card"
        :class="{ checked: selected.has(cat.key), unsupported: cat.implemented === false }"
        :title="cardTitle(cat)"
        @click="toggle(cat)"
      >
        <div class="card-icon" :style="{ background: colorFor(cat.key) }">
          <AppIcon :name="iconForCategory(cat.key)" :size="18" :stroke="2" />
        </div>
        <div class="card-name">{{ cat.name }}</div>
        <div class="card-size">{{ cat.size > 0 ? fmtSize(cat.size) : "—" }}</div>
        <div class="card-desc">{{ cat.describe || "" }}</div>
        <div class="tick"></div>
      </div>
    </div>
  </div>

  <div v-if="!categories.length && !scanningClean" class="empty">没有扫描到可清理项。</div>
</template>
