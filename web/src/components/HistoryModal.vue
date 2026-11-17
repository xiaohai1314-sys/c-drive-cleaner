<script setup lang="ts">
import { closeHistory, history, readHistory } from "../composables/useHistory";
import { fmtSize } from "../lib/format";
</script>

<template>
  <Teleport to="body">
    <div v-if="history.open" class="modal-mask">
      <div class="modal">
        <h3>清理历史</h3>
        <div class="modal-body">
          <template v-if="!history.detail">
            <div
              v-for="entry in history.entries.slice(0, 100)"
              :key="entry.name"
              class="history-row"
              @click="readHistory(entry.name)"
            >
              <span class="history-name">{{ entry.name }}</span>
              <span class="history-size">{{ fmtSize(entry.size) }}</span>
            </div>
          </template>
          <template v-else>
            <pre class="history-text">{{ history.detail }}</pre>
            <button class="btn btn-ghost" @click="history.detail = ''">返回列表</button>
          </template>
        </div>
        <div class="modal-actions">
          <button class="btn btn-primary" @click="closeHistory">关闭</button>
        </div>
      </div>
    </div>
  </Teleport>
</template>
