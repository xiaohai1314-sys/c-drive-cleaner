/** 格式化与文件分类工具，行为与原 app.js 保持一致。 */

export function fmtSize(bytes: number | null | undefined): string {
  const num = Number(bytes) || 0;
  if (num < 1024) return num + " B";
  let value = num;
  for (const unit of ["KB", "MB", "GB", "TB"]) {
    value /= 1024;
    if (value < 1024 || unit === "TB") return value.toFixed(1) + " " + unit;
  }
  return num + " B";
}

export function fmtTime(seconds: number): string {
  if (!seconds) return "—";
  const date = new Date(seconds * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ` +
    `${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

export function fmtDuration(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  const minutes = Math.floor(seconds / 60);
  return minutes > 0 ? `${minutes}:${String(seconds % 60).padStart(2, "0")}` : `${seconds} 秒`;
}

const VIDEO_EXT = new Set([".mp4", ".avi", ".mov", ".mkv", ".wmv", ".flv", ".m4v", ".3gp",
  ".mpeg", ".mpg", ".ts", ".webm", ".rmvb", ".vob"]);
const MUSIC_EXT = new Set([".mp3", ".m4a", ".wav", ".flac", ".aac", ".ogg", ".wma", ".ape", ".mid"]);
const IMAGE_EXT = new Set([".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp", ".heic", ".tif",
  ".tiff", ".ico", ".raw", ".psd", ".svg"]);
const DOC_EXT = new Set([".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".txt",
  ".csv", ".rtf", ".ofd", ".md", ".wps", ".et", ".dps", ".dwg", ".dxf"]);
const ARCHIVE_EXT = new Set([".zip", ".rar", ".7z", ".tar", ".gz", ".bz2", ".xz", ".iso", ".cab"]);

export function extOf(path: string): string {
  const name = String(path).split(/[\\/]/).pop() || "";
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot).toLowerCase() : "";
}

export function kindOf(path: string): string {
  const ext = extOf(path);
  if (VIDEO_EXT.has(ext)) return "video";
  if (MUSIC_EXT.has(ext)) return "music";
  if (IMAGE_EXT.has(ext)) return "image";
  if (DOC_EXT.has(ext)) return "document";
  if (ARCHIVE_EXT.has(ext)) return "archive";
  return "other";
}

export function diskOf(path: string): string {
  const text = String(path);
  return text.length >= 2 && text[1] === ":" ? text.slice(0, 2).toUpperCase() : "";
}

const PALETTE = ["#2f80ed", "#16a085", "#8e44ad", "#e67e22", "#4b6cb7", "#07c160",
  "#12b7f5", "#e74c3c", "#d35400", "#2980b9"];

export function colorFor(key: string): string {
  let sum = 0;
  for (const ch of String(key)) sum += ch.charCodeAt(0);
  return PALETTE[sum % PALETTE.length];
}
