<script setup lang="ts">
import { computed } from "vue";
import { SIDEBAR } from "../config/sidebar";
import { currentPage } from "../stores/app";

const label = computed(
  () => SIDEBAR.find((item) => item.key === currentPage.value)?.label || "",
);
</script>

<template>
  <div class="placeholder">
    <h2>{{ label }}</h2>
    <div>页面正在迁移中</div>
  </div>
</template>
