<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { invoke } from "../api/tauri";
import type { PrivacyItem, PrivacyReport } from "../api/types";
import { showConfirm, showResult } from "../composables/useModal";
import { toast } from "../composables/useToast";
import { privacyChecked, privacyItems, privacyLoading } from "../stores/privacy";

const SECTIONS: Array<[string, string, string]> = [
  ["high", "高危隐私", "容易泄露的个人敏感信息"],
  ["history", "上网浏览历史", "使用浏览器时产生的痕迹"],
  ["download", "下载历史记录", "浏览器下载记录，不删除已经下载的文件"],
  ["shop", "网络购物记录", "浏览记录里能识别的购物网站"],
  ["computer", "电脑使用痕迹", "使用电脑系统产生的痕迹"],
  ["software", "软件使用痕迹", "程序运行记录"],
  ["media", "影音播放痕迹", "播放器最近播放"],
];

const cleaning = ref(false);

const sections = computed(() =>
  SECTIONS.map(([key, title, note]) => ({
    key,
    title,
    note,
    list: privacyItems.value.filter((item) => item.section === key),
  })).filter((section) => section.list.length),
);

function selectable(item: PrivacyItem): boolean {
  return (
    item.action !== "count" &&
    (item.count_available !== false || (item.files && item.files.length > 0))
  );
}

function toggle(item: PrivacyItem) {
  if (!selectable(item)) return;
  if (privacyChecked.has(item.id)) privacyChecked.delete(item.id);
  else privacyChecked.add(item.id);
}

async function scan() {
  privacyLoading.value = true;
  try {
    privacyItems.value = await invoke<PrivacyItem[]>("privacy_scan");
  } catch (error) {
    privacyItems.value = [];
    toast("扫描失败：" + error);
  }
  privacyLoading.value = false;
  privacyChecked.clear();
  for (const item of privacyItems.value) {
    if (item.action !== "count" && item.count > 0 && item.count_available !== false) {
      privacyChecked.add(item.id);
    }
  }
}

async function clean() {
  const ids = [...privacyChecked];
  if (!ids.length) {
    toast("请先勾选要清理的项目");
    return;
  }

  let running: string[] = [];
  try {
    running = await invoke<string[]>("check_processes", {
      targets: ["chrome.exe", "msedge.exe", "firefox.exe", "qqbrowser.exe", "brave.exe"],
    });
  } catch (_) {
    running = [];
  }

  const lines = [
    `将清理 ${ids.length} 项隐私痕迹。`,
    "浏览器历史、Cookie、下载记录会被删除；文件类痕迹移到回收站；注册表项删除前会导出备份。",
    "已保存的密码不会被删除。",
  ];
  if (running.length) {
    lines.push(`检测到 ${running.join("、")} 正在运行，开始清理时会先关闭它们（未保存的网页表单内容会丢失）。`);
  }
  if (!(await showConfirm("确认清理隐私痕迹", lines, "开始清理"))) return;

  for (const proc of running) {
    try {
      await invoke("kill_process", { name: proc });
    } catch (_) {
      // 进程可能已经退出。
    }
  }

  cleaning.value = true;
  try {
    const report = await invoke<PrivacyReport>("privacy_clean", { ids });
    const result = [
      `文件 ${report.files} 个，数据库 ${report.sql} 个，注册表记录 ${report.registry} 条。`,
    ];
    for (const failure of (report.failed || []).slice(0, 5)) result.push(failure);
    await showResult("清理完成", result);
  } catch (error) {
    toast("清理失败：" + error);
  }
  cleaning.value = false;
  privacyItems.value = [];
  privacyChecked.clear();
  await scan();
}

onMounted(() => {
  if (!privacyItems.value.length && !privacyLoading.value) void scan();
});
</script>

<template>
  <div class="page-head">
    <div class="headline">隐私记录</div>
    <button class="green-btn" :disabled="cleaning" @click="clean">
      {{ cleaning ? "清理中…" : "一键清理" }}
    </button>
  </div>
  <div class="sub">
    {{ privacyLoading ? "正在扫描浏览器和注册表…" : `已选 ${privacyChecked.size} 项。已保存的密码不会被删除。` }}
  </div>

  <div v-for="section in sections" :key="section.key" class="section">
    <div class="section-title">
      <span>{{ section.title }}</span>
      <span class="note">{{ section.note }}</span>
    </div>
    <div class="cards">
      <div
        v-for="item in section.list"
        :key="item.id"
        class="pcard"
        :class="{
          checked: privacyChecked.has(item.id),
          disabled: !selectable(item),
        }"
        @click="toggle(item)"
      >
        <div class="check" :class="{ on: privacyChecked.has(item.id) }"></div>
        <div class="pbody">
          <div class="pname">{{ item.name }}</div>
          <div class="pcount">{{ item.count_available === false ? "—" : `${item.count} 条` }}</div>
          <div v-if="item.note" class="pnote">{{ item.note }}</div>
        </div>
      </div>
    </div>
  </div>

  <div v-if="!privacyItems.length && !privacyLoading" class="empty">没有扫描到隐私痕迹。</div>
</template>
