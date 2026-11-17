import { invoke } from "../api/tauri";
import { toast } from "../composables/useToast";

export async function revealFile(path: string) {
  try {
    await invoke("reveal_path", { path });
  } catch (error) {
    toast("定位失败：" + error);
  }
}

export async function openFile(path: string) {
  try {
    await invoke("open_path", { path });
  } catch (error) {
    toast("打开失败：" + error);
  }
}
