import { reactive } from "vue";
import { invoke } from "../api/tauri";
import type { HistoryEntry } from "../api/types";
import { showResult } from "./useModal";
import { toast } from "./useToast";

export const history = reactive({
  open: false,
  entries: [] as HistoryEntry[],
  detail: "",
});

export async function openHistory() {
  try {
    history.entries = await invoke<HistoryEntry[]>("history_list");
  } catch (error) {
    toast("读取历史失败：" + error);
    return;
  }
  if (!history.entries.length) {
    await showResult("清理历史", ["还没有清理记录。"]);
    return;
  }
  history.detail = "";
  history.open = true;
}

export async function readHistory(name: string) {
  try {
    history.detail = await invoke<string>("history_read", { name });
  } catch (error) {
    toast("读取失败：" + error);
  }
}

export function closeHistory() {
  history.open = false;
  history.detail = "";
}

export async function updateRules() {
  try {
    const message = await invoke<string>("rules_update");
    await showResult("规则更新", message.split("\n"));
  } catch (error) {
    await showResult("规则更新", [String(error)]);
  }
}

export async function rollbackRules() {
  try {
    const message = await invoke<string>("rules_rollback");
    await showResult("规则回滚", [message, "下次扫描生效。"]);
  } catch (error) {
    await showResult("规则回滚", [String(error)]);
  }
}
