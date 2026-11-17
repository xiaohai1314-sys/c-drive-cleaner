import { ref } from "vue";
import type { HiberInfo, SystemItem } from "../api/types";

export const systemItems = ref<SystemItem[]>([]);
export const systemLoading = ref(false);
export const hiber = ref<HiberInfo | null>(null);
