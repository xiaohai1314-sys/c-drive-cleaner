import { ref } from "vue";
import { invoke } from "../api/tauri";
import type { ScanStatus } from "../api/types";
import { toast } from "./useToast";

export const scanStatus = ref<Record<string, ScanStatus> | null>(null);

let timer: number | undefined;
let users = 0;

/** 多个页面可能同时需要扫描进度，按引用计数启停。 */
export function startScanStatus() {
  users += 1;
  if (timer !== undefined) return;
  timer = window.setInterval(async () => {
    try {
      scanStatus.value = await invoke<Record<string, ScanStatus>>("scan_status");
    } catch (_) {
      // 浏览器预览时没有 Tauri，安静跳过。
    }
  }, 500);
}

export function stopScanStatus() {
  users = Math.max(0, users - 1);
  if (users > 0) return;
  if (timer !== undefined) window.clearInterval(timer);
  timer = undefined;
  scanStatus.value = null;
}

export async function cancelActiveScan(kind: string) {
  try {
    await invoke("cancel_scan", { kind });
  } catch (error) {
    toast("取消失败：" + error);
  }
}
