<script setup lang="ts">
import AppIcon from "./AppIcon.vue";
import { SIDEBAR } from "../config/sidebar";
import { openHistory, rollbackRules, updateRules } from "../composables/useHistory";
import { currentPage, navSizes, setPage } from "../stores/app";
</script>

<template>
  <aside class="sidebar">
    <div class="brand" data-tauri-drag-region>
      <div class="logo">C</div>
      <span>磁盘清理助手</span>
    </div>
    <nav id="nav" class="nav">
      <div
        v-for="item in SIDEBAR"
        :key="item.key"
        class="nav-item"
        :class="{ active: currentPage === item.key }"
        @click="setPage(item.key as never)"
      >
        <div class="nav-icon" :style="{ background: item.color }">
          <AppIcon :name="item.icon" :size="15" :stroke="2" />
        </div>
        <span>{{ item.label }}</span>
        <span class="nav-size">{{ navSizes[item.key] || "" }}</span>
      </div>
    </nav>
    <div class="sidebar-bottom">
      <a href="#" @click.prevent="openHistory">清理历史</a>
      <a href="#" @click.prevent="updateRules">规则更新</a>
      <a href="#" @click.prevent="rollbackRules">规则回滚</a>
    </div>
  </aside>
</template>
