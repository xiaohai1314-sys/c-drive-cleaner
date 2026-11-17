/** 对 window.__TAURI__ 的一层薄封装：懒读取，便于浏览器预览时优雅降级。 */

interface TauriCore {
  invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
  convertFileSrc?: (path: string, protocol?: string) => string;
}

interface TauriWindowHandle {
  minimize(): void;
  toggleMaximize(): void;
  close(): void;
}

interface TauriGlobal {
  core?: TauriCore;
  window?: { getCurrentWindow?: () => TauriWindowHandle };
  dialog?: { open?: (options: Record<string, unknown>) => Promise<unknown> };
}

function globalTauri(): TauriGlobal | undefined {
  return (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__;
}

export function hasTauri(): boolean {
  return Boolean(globalTauri()?.core?.invoke);
}

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const fn = globalTauri()?.core?.invoke;
  if (!fn) throw new Error("当前不在 Tauri 环境中");
  return (await fn(cmd, args)) as T;
}

export function convertFileSrc(path: string, protocol = "asset"): string {
  const fn = globalTauri()?.core?.convertFileSrc;
  return fn ? fn(path, protocol) : "";
}

export function currentWindow(): TauriWindowHandle | undefined {
  return globalTauri()?.window?.getCurrentWindow?.();
}

export async function pickPaths(kind: "file" | "folder"): Promise<string[]> {
  const open = globalTauri()?.dialog?.open;
  if (!open) return [];
  const picked = await open(
    kind === "file"
      ? { multiple: true, title: "选择要粉碎的文件" }
      : { directory: true, multiple: true, title: "选择要粉碎的文件夹" },
  );
  if (!picked) return [];
  return Array.isArray(picked) ? (picked as string[]) : [picked as string];
}
