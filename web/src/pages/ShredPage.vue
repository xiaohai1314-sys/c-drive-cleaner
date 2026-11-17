<script setup lang="ts">
import { ref } from "vue";
import { invoke, pickPaths } from "../api/tauri";
import { showConfirm, showResult } from "../composables/useModal";
import { toast } from "../composables/useToast";
import { fmtSize } from "../lib/format";
import { shredPaths } from "../stores/shred";

interface ShredReport {
  files_ok: number;
  files_failed: number;
  freed: number;
  errors: string[];
}

const running = ref(false);

async function add(kind: "file" | "folder") {
  try {
    const picked = await pickPaths(kind);
    for (const path of picked) {
      if (!shredPaths.value.includes(path)) shredPaths.value.push(path);
    }
  } catch (error) {
    toast("选择失败：" + error);
  }
}

function remove(path: string) {
  shredPaths.value = shredPaths.value.filter((item) => item !== path);
}

async function run() {
  if (!shredPaths.value.length) {
    toast("请先添加要粉碎的文件");
    return;
  }
  const lines = [
    `将粉碎 ${shredPaths.value.length} 个路径。`,
    "粉碎会覆盖写入后删除，不走回收站，删除后无法从本工具找回。",
    "固态盘（SSD）上无法保证旧数据在物理层面不可恢复。",
    "系统目录和受保护路径会被拒绝。",
  ];
  if (!(await showConfirm("确认粉碎", lines, "开始粉碎"))) return;
  running.value = true;
  try {
    const report = await invoke<ShredReport>("shred", { paths: shredPaths.value, passes: 1 });
    const result = [
      `成功 ${report.files_ok} 个，失败 ${report.files_failed} 个，释放 ${fmtSize(report.freed)}。`,
    ];
    for (const error of (report.errors || []).slice(0, 5)) result.push(error);
    await showResult("粉碎完成", result);
    shredPaths.value = [];
  } catch (error) {
    toast("粉碎失败：" + error);
  }
  running.value = false;
}
</script>

<template>
  <div class="page-head">
    <div class="headline">彻底粉碎文件，远离残留</div>
    <button class="green-btn" :disabled="running" @click="run">
      {{ running ? "粉碎中…" : "一键粉碎" }}
    </button>
  </div>
  <div class="sub">
    先加入列表，确认后用随机数据覆盖再删除，不走回收站。机械盘上基本无法恢复；固态盘因磨损均衡无法保证，敏感数据请配合 BitLocker。
  </div>

  <div class="shred-actions">
    <button class="btn btn-ghost" @click="add('file')">添加文件</button>
    <button class="btn btn-ghost" @click="add('folder')">添加文件夹</button>
  </div>

  <div v-if="!shredPaths.length" class="empty">
    还没有添加文件。点击上方按钮选择需要粉碎的文件或文件夹。
  </div>
  <div v-else class="shred-list">
    <div v-for="path in shredPaths" :key="path" class="shred-row">
      <div class="name" :title="path">{{ path }}</div>
      <div class="shred-remove" @click="remove(path)">移除</div>
    </div>
  </div>
</template>
