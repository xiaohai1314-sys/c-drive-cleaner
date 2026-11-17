import { reactive, ref, shallowRef, type Ref, type ShallowRef } from "vue";
import type { ImRecord, MoveAnalysis } from "../api/types";
import type { ImKind } from "../config/sidebar";

export interface ImStore {
  files: ShallowRef<ImRecord[]>;
  loading: Ref<boolean>;
  tab: Ref<string>;
  checked: Set<string>;
  move: Ref<MoveAnalysis | null>;
}

function create(): ImStore {
  return {
    files: shallowRef<ImRecord[]>([]),
    loading: ref(false),
    tab: ref("video"),
    checked: reactive(new Set<string>()),
    move: ref<MoveAnalysis | null>(null),
  };
}

export const imStores: Record<ImKind, ImStore> = {
  wechat: create(),
  qq: create(),
};
