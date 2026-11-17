<script setup lang="ts">
import { computed, onMounted, watchEffect, type Component } from "vue";
import AppSidebar from "./components/AppSidebar.vue";
import WindowBar from "./components/WindowBar.vue";
import TaskDock from "./components/TaskDock.vue";
import ToastHost from "./components/ToastHost.vue";
import ModalHost from "./components/ModalHost.vue";
import HistoryModal from "./components/HistoryModal.vue";
import CleanPage from "./pages/CleanPage.vue";
import SystemPage from "./pages/SystemPage.vue";
import FragPage from "./pages/FragPage.vue";
import PrivacyPage from "./pages/PrivacyPage.vue";
import BigPage from "./pages/BigPage.vue";
import DupPage from "./pages/DupPage.vue";
import WechatPage from "./pages/WechatPage.vue";
import QqPage from "./pages/QqPage.vue";
import ShredPage from "./pages/ShredPage.vue";
import { currentPage, loadDisk, loadDrives, setNavSize, type PageKey } from "./stores/app";
import { bigRows } from "./stores/big";
import { categories, selected } from "./stores/clean";
import { dupGroups } from "./stores/dup";
import { fmtSize } from "./lib/format";
import { startTaskPolling } from "./stores/tasks";

const pages: Record<PageKey, Component> = {
  clean: CleanPage,
  system: SystemPage,
  frag: FragPage,
  privacy: PrivacyPage,
  big: BigPage,
  dup: DupPage,
  wechat: WechatPage,
  qq: QqPage,
  shred: ShredPage,
};

const pageComponent = computed(() => pages[currentPage.value]);

watchEffect(() => {
  const cleanTotal = categories.value
    .filter((cat) => selected.has(cat.key))
    .reduce((sum, cat) => sum + Math.max(cat.size, 0), 0);
  setNavSize("clean", cleanTotal > 0 ? `[${fmtSize(cleanTotal)}]` : "");

  const bigTotal = bigRows.value.reduce((sum, row) => sum + row.size, 0);
  setNavSize("big", bigTotal > 0 ? `[${fmtSize(bigTotal)}]` : "");

  const dupTotal = dupGroups.value.reduce(
    (sum, group) => sum + group.size * Math.max(group.files.length - 1, 0),
    0,
  );
  setNavSize("dup", dupTotal > 0 ? `[${fmtSize(dupTotal)}]` : "");
});

onMounted(() => {
  void loadDisk();
  void loadDrives();
  startTaskPolling();
});
</script>

<template>
  <div class="app">
    <AppSidebar />
    <main class="main">
      <WindowBar />
      <section id="page" class="page">
        <component :is="pageComponent" :key="currentPage" />
      </section>
    </main>
    <TaskDock />
    <ToastHost />
    <ModalHost />
    <HistoryModal />
  </div>
</template>
