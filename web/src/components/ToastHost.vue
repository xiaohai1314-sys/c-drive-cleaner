<script setup lang="ts">
import { toastShown, toastText } from "../composables/useToast";
</script>

<template>
  <Teleport to="body">
    <div class="toast" :class="{ show: toastShown }">{{ toastText }}</div>
  </Teleport>
</template>
