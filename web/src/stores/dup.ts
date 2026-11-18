import { reactive, ref, shallowRef } from "vue";
import type { DupGroup } from "../api/types";

export type DupScope = "C:\\" | "USER" | "ALL" | "PICK";

export const dupGroups = shallowRef<DupGroup[]>([]);
export const dupLoading = ref(false);
export const dupScope = ref<DupScope>("C:\\");
export const dupDrives = reactive(new Set<string>());
export const dupChecked = reactive(new Set<string>());
/** 当前分组来自哪次扫描（unix 秒）；0 表示还没扫过。 */
export const dupCachedAt = ref(0);
