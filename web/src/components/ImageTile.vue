<script setup lang="ts">
import { computed, ref } from "vue";
import { convertFileSrc } from "../api/tauri";
import type { ImRecord } from "../api/types";
import { revealFile } from "../lib/files";
import { fmtSize, fmtTime } from "../lib/format";

const props = defineProps<{ file: ImRecord; checked: boolean }>();
const emit = defineEmits<{ (event: "toggle"): void }>();

const failed = ref(false);
const url = computed(() => convertFileSrc(props.file.path, "imthumb"));
const tip = computed(
  () => `${props.file.name}\n${fmtSize(props.file.size)} · ${fmtTime(props.file.mtime)}\n单击选中，双击定位`,
);
</script>

<template>
  <div
    class="im-tile"
    :class="{ on: checked }"
    :title="tip"
    @click="emit('toggle')"
    @dblclick="revealFile(file.path)"
  >
    <div class="thumb" :class="{ failed }">
      <img
        v-if="url && !failed"
        :src="url"
        loading="lazy"
        decoding="async"
        alt=""
        @error="failed = true"
      />
    </div>
    <div class="check" :class="{ on: checked }" @click.stop="emit('toggle')"></div>
    <div class="cap">{{ fmtSize(file.size) }}</div>
  </div>
</template>
