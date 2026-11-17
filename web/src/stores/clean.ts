import { reactive, ref } from "vue";
import { invoke } from "../api/tauri";
import type { CategorySize } from "../api/types";
import { toast } from "../composables/useToast";

export const categories = ref<CategorySize[]>([]);
export const scanningClean = ref(false);
export const selected = reactive(new Set<string>());

export function selectedSize(): number {
  return categories.value
    .filter((cat) => selected.has(cat.key))
    .reduce((sum, cat) => sum + Math.max(cat.size, 0), 0);
}

export async function scanCategories() {
  scanningClean.value = true;
  try {
    categories.value = await invoke<CategorySize[]>("scan_categories");
  } catch (error) {
    categories.value = [];
    toast("扫描失败：" + error);
  }
  scanningClean.value = false;
  // 默认勾选推荐项，和 Python 版一致。
  for (const cat of categories.value) {
    if (cat.implemented !== false && !cat.requires_confirm && cat.tier !== "deep") {
      selected.add(cat.key);
    }
  }
}
