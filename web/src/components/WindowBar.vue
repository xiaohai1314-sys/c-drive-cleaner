<script setup lang="ts">
import { currentWindow } from "../api/tauri";

const win = currentWindow();

function minimize() {
  win?.minimize();
}
function toggleMaximize() {
  win?.toggleMaximize();
}
function close() {
  win?.close();
}
</script>

<template>
  <div class="topbar" data-tauri-drag-region>
    <span class="spacer" data-tauri-drag-region></span>
    <button class="win-btn" title="最小化" @click="minimize">—</button>
    <button class="win-btn" title="最大化" @click="toggleMaximize">□</button>
    <button class="win-btn" title="关闭" @click="close">✕</button>
  </div>
</template>
