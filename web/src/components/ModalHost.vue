<script setup lang="ts">
import { nextTick, ref, watch } from "vue";
import { closeModal, modal } from "../composables/useModal";
import { fmtSize } from "../lib/format";

const okButton = ref<HTMLButtonElement | null>(null);

watch(
  () => modal.open,
  async (open) => {
    if (!open) return;
    await nextTick();
    okButton.value?.focus();
  },
);
</script>

<template>
  <Teleport to="body">
    <div v-if="modal.open" class="modal-mask">
      <div class="modal" :class="{ 'preview-modal': Boolean(modal.preview) }">
        <h3>{{ modal.title }}</h3>
        <div class="modal-body">
          <template v-if="modal.preview">
            <p v-if="modal.preview.binary">
              微信的日志是加密压缩的二进制格式，没法直接读，下面是文件开头的十六进制内容。
            </p>
            <pre class="preview-text">{{ modal.preview.text || "（空文件）" }}</pre>
            <p v-if="modal.preview.truncated" class="muted">
              只显示了开头部分，文件共 {{ fmtSize(modal.preview.size) }}。
            </p>
          </template>
          <template v-else>
            <p v-for="(line, index) in modal.lines" :key="index">{{ line }}</p>
          </template>
        </div>
        <div class="modal-actions">
          <button v-if="modal.cancelText" class="btn btn-ghost" @click="closeModal(false)">
            {{ modal.cancelText }}
          </button>
          <button ref="okButton" class="btn btn-primary" @click="closeModal(true)">
            {{ modal.confirmText || "知道了" }}
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>
