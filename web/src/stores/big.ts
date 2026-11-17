import { reactive, ref, shallowRef } from "vue";
import type { BigFile } from "../api/types";
import type { BigKind } from "../config/sidebar";

export const BIG_PAGE = 500;

export const bigRows = shallowRef<BigFile[]>([]);
export const bigLoading = ref(false);
export const bigTab = ref<BigKind>("all");
export const bigShown = ref(BIG_PAGE);
export const bigChecked = reactive(new Set<string>());
