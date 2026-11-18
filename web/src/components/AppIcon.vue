<script setup lang="ts">
import { computed } from "vue";
import { ICONS } from "../lib/icons";

const props = withDefaults(defineProps<{ name: string; size?: number; stroke?: number }>(), {
  size: 20,
  stroke: 1.8,
});

const markup = computed(() => ICONS[props.name] ?? ICONS.folder);
</script>

<template>
  <svg
    class="icon"
    :width="size"
    :height="size"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    :stroke-width="stroke"
    stroke-linecap="round"
    stroke-linejoin="round"
    v-html="markup"
  />
</template>
