import { ref } from "vue";
import type { FragReport } from "../api/types";

export const fragReport = ref<FragReport | null>(null);
export const fragLoading = ref(false);
export const fragDrive = ref("C:\\");
