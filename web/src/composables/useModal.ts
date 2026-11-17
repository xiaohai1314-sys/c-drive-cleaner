import { reactive } from "vue";
import type { ImPreview } from "../api/types";

interface ModalState {
  open: boolean;
  title: string;
  lines: string[];
  confirmText: string;
  cancelText: string;
  preview: ImPreview | null;
  resolve: ((ok: boolean) => void) | null;
}

export const modal = reactive<ModalState>({
  open: false,
  title: "",
  lines: [],
  confirmText: "知道了",
  cancelText: "",
  preview: null,
  resolve: null,
});

function open(state: Partial<ModalState>): Promise<boolean> {
  return new Promise((resolve) => {
    Object.assign(modal, {
      open: true,
      title: "",
      lines: [],
      confirmText: "知道了",
      cancelText: "",
      preview: null,
      ...state,
      resolve,
    });
  });
}

export function showConfirm(
  title: string,
  lines: string[],
  confirmText: string,
  cancelText = "取消",
): Promise<boolean> {
  return open({ title, lines, confirmText, cancelText });
}

export function showResult(title: string, lines: string[]): Promise<boolean> {
  return open({ title, lines });
}

export function showPreview(path: string, preview: ImPreview): Promise<boolean> {
  const name = path.split(/[\\/]/).pop() || path;
  return open({ title: name, preview });
}

export function closeModal(ok: boolean) {
  const resolve = modal.resolve;
  modal.open = false;
  modal.resolve = null;
  resolve?.(ok);
}
