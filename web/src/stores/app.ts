import { ref } from "vue";
import { invoke } from "../api/tauri";
import type { DiskInfo } from "../api/types";

export type PageKey =
  | "clean" | "system" | "frag" | "privacy" | "big"
  | "dup" | "wechat" | "qq" | "shred";

export const currentPage = ref<PageKey>("clean");
export const disk = ref<DiskInfo | null>(null);
export const drives = ref<string[]>([]);
/** 侧边栏右侧的占用标记，键是页面 key。 */
export const navSizes = ref<Record<string, string>>({});

export function setPage(page: PageKey) {
  currentPage.value = page;
}

export function setNavSize(key: string, text: string) {
  if ((navSizes.value[key] || "") === text) return;
  navSizes.value = { ...navSizes.value, [key]: text };
}

export async function loadDisk() {
  try {
    disk.value = await invoke<DiskInfo>("disk_info");
  } catch (_) {
    disk.value = null;
  }
}

export async function loadDrives() {
  try {
    drives.value = await invoke<string[]>("list_drives");
  } catch (_) {
    drives.value = [];
  }
}
