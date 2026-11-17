import { reactive, ref } from "vue";
import type { PrivacyItem } from "../api/types";

export const privacyItems = ref<PrivacyItem[]>([]);
export const privacyLoading = ref(false);
export const privacyChecked = reactive(new Set<string>());
