export interface SidebarItem {
  key: string;
  label: string;
  symbol: string;
  color: string;
}

export const SIDEBAR: SidebarItem[] = [
  { key: "clean", label: "C盘瘦身", symbol: "C", color: "#2f80ed" },
  { key: "system", label: "系统深度瘦身", symbol: "系", color: "#9b59b6" },
  { key: "frag", label: "碎片清理", symbol: "碎", color: "#16a085" },
  { key: "privacy", label: "隐私清理", symbol: "隐", color: "#8e44ad" },
  { key: "big", label: "大文件专清", symbol: "大", color: "#e67e22" },
  { key: "dup", label: "重复文件专清", symbol: "重", color: "#4b6cb7" },
  { key: "wechat", label: "微信专清", symbol: "微", color: "#07c160" },
  { key: "qq", label: "QQ专清", symbol: "Q", color: "#12b7f5" },
  { key: "shred", label: "文件粉碎", symbol: "粉", color: "#e74c3c" },
];

export const BIG_KINDS = ["all", "video", "music", "document", "image", "archive", "other"] as const;
export type BigKind = (typeof BIG_KINDS)[number];

export const BIG_NAMES: Record<BigKind, string> = {
  all: "全部",
  video: "视频",
  music: "音乐",
  document: "文档",
  image: "图片",
  archive: "压缩包",
  other: "其他",
};

export type ImKind = "wechat" | "qq";

export const IM_KINDS: Record<ImKind, Array<[string, string]>> = {
  wechat: [
    ["video", "聊天视频"],
    ["image", "聊天图片"],
    ["file", "接收文件"],
    ["cache", "缓存"],
    ["log", "日志"],
    ["sns", "朋友圈"],
  ],
  qq: [
    ["video", "聊天视频"],
    ["image", "聊天图片"],
    ["cache", "缓存"],
    ["audio", "聊天语音"],
    ["file", "接收文件"],
  ],
};
