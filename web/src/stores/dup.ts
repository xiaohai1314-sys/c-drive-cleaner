import { reactive, ref, shallowRef } from "vue";
import type { DupGroup } from "../api/types";

export type DupScope = "C:\\" | "USER" | "ALL" | "PICK";

export const dupGroups = shallowRef<DupGroup[]>([]);
export const dupLoading = ref(false);
export const dupScope = ref<DupScope>("C:\\");
export const dupDrives = reactive(new Set<string>());
export const dupFast = ref(false);
export const dupChecked = reactive(new Set<string>());
