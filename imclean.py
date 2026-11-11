"""微信 / QQ 深度清理：按类型扫描媒体文件，只删媒体，不碰数据库和配置。"""
import os

import utils

IMAGE_EXT = {".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp", ".heic", ".tif", ".tiff"}
VIDEO_EXT = {".mp4", ".avi", ".mov", ".mkv", ".wmv", ".flv", ".m4v", ".3gp"}
AUDIO_EXT = {".silk", ".amr", ".mp3", ".m4a", ".wav", ".ogg", ".aac"}
FILE_EXT = {".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".txt",
            ".csv", ".zip", ".rar", ".7z", ".dwg", ".ofd", ".psd", ".ai"}
SKIP_EXT = {".db", ".db-wal", ".db-shm", ".sqlite", ".ini", ".dat", ".key",
            ".xml", ".json", ".pfx", ".crt", ".exe", ".dll"}

KINDS = [
    ("image", "聊天图片"),
    ("video", "聊天视频"),
    ("audio", "语音/音频"),
    ("file", "接收文件"),
    ("cache", "缓存"),
    ("log", "日志"),
]


def _roots(names):
    local = os.environ.get("LOCALAPPDATA", "")
    roaming = os.environ.get("APPDATA", "")
    profile = os.environ.get("USERPROFILE", "")
    candidates = []
    for kind, sub in names:
        base = {"local": local, "roaming": roaming, "profile": profile}[kind]
        candidates.append(os.path.join(base, sub))
    return [path for path in candidates if os.path.isdir(path)]


def wechat_roots():
    return _roots([
        ("roaming", r"Tencent\WeChat"),
        ("roaming", r"Tencent\xwechat"),
        ("profile", r"Documents\WeChat Files"),
        ("profile", r"Documents\xwechat_files"),
    ])


def qq_roots():
    return _roots([
        ("roaming", r"Tencent\QQ"),
        ("roaming", r"Tencent\QQNT"),
        ("profile", r"Documents\Tencent Files"),
    ])


def _classify(path):
    low = path.lower()
    if "\\cache" in low or "\\temp" in low:
        return "cache"
    if "\\log" in low:
        return "log"
    ext = os.path.splitext(path)[1].lower()
    if ext in IMAGE_EXT:
        return "image"
    if ext in VIDEO_EXT:
        return "video"
    if ext in AUDIO_EXT:
        return "audio"
    if ext in FILE_EXT:
        return "file"
    return None


def scan(roots):
    result = {key: {"size": 0, "count": 0} for key, _ in KINDS}
    for root in roots:
        for entry in utils.walk_files(root):
            ext = os.path.splitext(entry.path)[1].lower()
            if ext in SKIP_EXT:
                continue
            kind = _classify(entry.path)
            if not kind:
                continue
            try:
                size = entry.stat(follow_symlinks=False).st_size
            except OSError:
                continue
            result[kind]["size"] += size
            result[kind]["count"] += 1
    return result


def clean(roots, kinds):
    freed = 0
    deleted = 0
    for root in roots:
        for entry in utils.walk_files(root):
            ext = os.path.splitext(entry.path)[1].lower()
            if ext in SKIP_EXT:
                continue
            kind = _classify(entry.path)
            if kind not in kinds:
                continue
            try:
                size = entry.stat(follow_symlinks=False).st_size
                os.remove(entry.path)
                freed += size
                deleted += 1
            except OSError:
                continue
    return freed, deleted
