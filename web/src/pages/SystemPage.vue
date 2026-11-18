<script setup lang="ts">
import { onMounted, ref } from "vue";
import AppIcon from "../components/AppIcon.vue";
import { invoke } from "../api/tauri";
import type { HiberInfo, SystemItem } from "../api/types";
import { showConfirm, showResult } from "../composables/useModal";
import { toast } from "../composables/useToast";
import { fmtSize } from "../lib/format";
import { loadDisk } from "../stores/app";
import { hiber, systemItems, systemLoading } from "../stores/system";

const busyKey = ref<string | null>(null);
const emptying = ref(false);

async function scan() {
  systemLoading.value = true;
  try {
    systemItems.value = await invoke<SystemItem[]>("system_scan");
    hiber.value = await invoke<HiberInfo>("hiber_query");
  } catch (error) {
    toast("系统项扫描失败：" + error);
  }
  systemLoading.value = false;
}

async function emptyRecycle() {
  const ok = await showConfirm(
    "确认清空回收站",
    ["将彻底清空所有驱动器中的回收站，清空后文件无法还原。"],
    "立即清空",
  );
  if (!ok) return;
  emptying.value = true;
  try {
    await invoke("recycle_empty");
    toast("全局回收站已清空！");
    await loadDisk();
  } catch (error) {
    toast("清空回收站失败：" + error);
  }
  emptying.value = false;
}

async function runItem(item: SystemItem) {
  const lines = item.is_command
    ? [
        "将执行 DISM 组件库精简，耗时约 1~3 分钟，期间请勿关闭程序。",
        "只会清理 WinSxS 里的历史更新备份，已安装的更新和系统功能不受影响。",
      ]
    : [
        `将删除「${item.name}」中的 ${item.count} 个文件，约 ${fmtSize(item.size)}。`,
        "这些都是系统缓存、更新残留或崩溃日志，删除后不影响系统正常运行。",
      ];
  const ok = await showConfirm(
    item.is_command ? "确认执行组件库精简" : `确认清理${item.name}`,
    lines,
    item.is_command ? "开始精简" : "立即清理",
  );
  if (!ok) return;
  busyKey.value = item.key;
  try {
    const message = await invoke<string>("system_clean", { key: item.key });
    await showResult("清理完成", [message]);
    await scan();
    await loadDisk();
  } catch (error) {
    toast("执行失败：" + error);
  }
  busyKey.value = null;
}

async function setHiberMode(mode: "off" | "reduced" | "full") {
  const size = hiber.value?.size ? fmtSize(hiber.value.size) : "全部空间";
  if (mode === "off") {
    const ok = await showConfirm(
      "确认彻底关闭休眠",
      [
        `hiberfil.sys（约 ${size}）会被删除，C 盘空间立即释放。`,
        "关闭休眠会同时关闭 Windows「快速启动」，开机可能略慢几秒。",
        "「睡眠」功能不受影响；需要时随时可以重新开启休眠。",
      ],
      "关闭休眠",
    );
    if (!ok) return;
  } else if (mode === "reduced") {
    const ok = await showConfirm(
      "确认精简休眠文件",
      [`休眠文件会缩减到约一半（当前 ${size}）。`, "仍可正常休眠，但内核内存转储会受限。"],
      "精简休眠文件",
    );
    if (!ok) return;
  }
  try {
    const message = await invoke<string>("hiber_set", { mode });
    toast(message);
    hiber.value = await invoke<HiberInfo>("hiber_query");
    await loadDisk();
  } catch (error) {
    toast("设置休眠失败：" + error);
  }
}

onMounted(() => {
  if (!systemItems.value.length && !systemLoading.value) void scan();
});
</script>

<template>
  <div class="page-head">
    <div class="headline">Windows 系统深度瘦身</div>
    <button class="green-btn" :disabled="emptying" @click="emptyRecycle">清空回收站</button>
  </div>
  <div class="sub">清理 Windows 系统更新残留、传递优化网络分片、崩溃日志与休眠文件</div>

  <div class="section">
    <div class="section-title">
      <span>系统深度组件</span>
      <span class="note">Windows 核心缓存与更新冗余</span>
    </div>

    <div v-if="!systemItems.length" class="empty">
      {{ systemLoading ? "正在分析系统更新与组件库…" : "未扫描到系统更新垃圾。" }}
    </div>

    <div v-for="item in systemItems" :key="item.key" class="sys-card">
      <div class="card-icon" style="background: #9b59b6"><AppIcon name="gear" :size="18" :stroke="2" /></div>
      <div class="sys-info">
        <div class="sys-title">{{ item.name }}</div>
        <div class="sys-desc">{{ item.describe }}</div>
      </div>
      <div v-if="item.size > 0" class="sys-size">{{ fmtSize(item.size) }}</div>
      <button
        v-if="!item.is_command && item.count === 0"
        class="smart-btn primary"
        disabled
        style="opacity: 0.55"
      >
        无需清理
      </button>
      <button
        v-else
        class="smart-btn primary"
        :disabled="busyKey === item.key"
        @click="runItem(item)"
      >
        {{ busyKey === item.key ? "处理中…" : item.is_command ? "执行精简" : "立即清理" }}
      </button>
    </div>
  </div>

  <div v-if="hiber" class="section">
    <div class="section-title">
      <span>系统休眠管理 (hiberfil.sys)</span>
      <span class="note">台式机/笔记本空间释放绝技</span>
    </div>
    <div class="sys-card">
      <div class="card-icon" style="background: #e67e22"><AppIcon name="moon" :size="18" :stroke="2" /></div>
      <div class="sys-info">
        <div class="sys-title">休眠状态：{{ hiber.enabled ? "已开启" : "未开启 / 已释放" }}</div>
        <div class="sys-desc">{{ hiber.description }}</div>
      </div>
      <div v-if="hiber.size > 0" class="sys-size">{{ fmtSize(hiber.size) }}</div>
      <div class="smart-tools">
        <template v-if="hiber.enabled">
          <button class="smart-btn" @click="setHiberMode('off')">彻底关闭休眠（释放全部空间）</button>
          <button class="smart-btn" @click="setHiberMode('reduced')">精简模式（Reduced，缩减50%）</button>
        </template>
        <button v-else class="smart-btn primary" @click="setHiberMode('full')">开启休眠</button>
      </div>
    </div>
  </div>
</template>
