"""微信 / QQ 深度清理。

只识别媒体、缓存和日志。聊天数据库、配置和密码类文件一律跳过。
新版微信的图片和视频经常是 .dat 或没有扩展名，只在明确的媒体目录里才算。
"""
import os
import time

import filetypes
import utils

IMAGE_EXT = filetypes.IMAGE_EXT
VIDEO_EXT = filetypes.VIDEO_EXT
AUDIO_EXT = {".silk", ".amr", ".mp3", ".m4a", ".wav", ".ogg", ".aac", ".aud"}
FILE_EXT = filetypes.DOC_EXT | filetypes.ARCHIVE_EXT | {".psd", ".ai"}
SKIP_EXT = {".db", ".db-wal", ".db-shm", ".sqlite", ".sqlite3", ".ini", ".key",
            ".xml", ".json", ".pfx", ".crt", ".exe", ".dll", ".bin", ".cfg"}

# 这些目录里的 .dat / 无扩展名文件是聊天数据库或配置，不是可清理的媒体。
SKIP_DIR_PARTS = {
    "db_storage", "config", "account", "apputil", "login",
    "db", "database", "databases",
}

KINDS = [
    ("image", "聊天图片"),
    ("video", "聊天视频"),
    ("audio", "语音/音频"),
    ("file", "接收文件"),
    ("cache", "缓存"),
    ("log", "日志"),
    ("sns", "朋友圈"),
]

WECHAT_TABS = [
    ("video", "聊天视频"),
    ("image", "聊天图片"),
    ("file", "接收文件"),
    ("cache", "缓存"),
    ("log", "日志"),
    ("sns", "朋友圈"),
]
QQ_TABS = [
    ("video", "聊天视频"),
    ("image", "聊天图片"),
    ("cache", "缓存"),
    ("audio", "聊天语音"),
    ("file", "接收文件"),
]
FILE_SUBS = [
    ("doc", "办公文档"),
    ("video", "视频"),
    ("image", "图片"),
    ("music", "音乐"),
    ("archive", "压缩包"),
    ("other", "其他"),
]

_MEDIA_DIRS = {
    "video": {"video", "videos"},
    "image": {"image", "images", "img", "attach", "msgattach", "custom_emotion", "emotion"},
    "audio": {"voice", "audio", "ptt", "sound"},
    "file": {"file", "files", "filerecv", "file_recv", "fav", "favorite"},
    "sns": {"sns", "moments", "timeline"},
    "cache": {"cache", "temp", "emoji", "emoticon", "cdncomm"},
    "log": {"log", "logs", "xlog", "crash", "crashinfo"},
}


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


def _parts(path):
    text = str(path).replace("/", "\\").lower()
    return [part for part in text.split("\\") if part]


def _dir_kind(dir_parts):
    """目录段决定 .dat 和无扩展名文件的类型。更具体的媒体目录优先于缓存。"""
    parts = set(dir_parts)
    for kind in ("sns", "video", "image", "audio", "file", "log", "cache"):
        if parts & _MEDIA_DIRS[kind]:
            return kind
    return None


def classify(path):
    """返回 (kind, sub)。不能清理时 kind 为 None。"""
    parts = _parts(path)
    if not parts:
        return None, ""
    ext = os.path.splitext(parts[-1])[1]
    if ext in SKIP_EXT:
        return None, ""
    dirs = parts[:-1]
    if set(dirs) & SKIP_DIR_PARTS:
        return None, ""
    dir_kind = _dir_kind(dirs)
    # 目录语义优先：接收文件目录里的视频仍归「接收文件」，缓存目录里的图片仍归缓存。
    # 没有目录语义时，.dat 和无扩展名一律不猜，避免扫到配置。
    if dir_kind:
        kind = dir_kind
    elif ext in ("", ".dat", ".tmp"):
        kind = None
    elif ext in VIDEO_EXT:
        kind = "video"
    elif ext in IMAGE_EXT:
        kind = "image"
    elif ext in AUDIO_EXT:
        kind = "audio"
    elif ext in FILE_EXT or ext in filetypes.ARCHIVE_EXT or ext in filetypes.DOC_EXT:
        kind = "file"
    elif ext in {".log", ".xlog", ".dmp"}:
        kind = "log"
    else:
        kind = None
    if not kind:
        return None, ""
    sub = filetypes.received_subkind(path) if kind == "file" else ""
    return kind, sub


def _entry_record(path, size, mtime):
    kind, sub = classify(path)
    if not kind:
        return None
    stamp = time.localtime(mtime or 0)
    label = dict(KINDS).get(kind, "")
    return {
        "path": path,
        "size": int(size or 0),
        "mtime": mtime or 0,
        "kind": kind,
        "sub": sub,
        "month": f"{stamp.tm_year:04d}-{stamp.tm_mon:02d}",
        "month_label": f"{stamp.tm_year}年{stamp.tm_mon}月{label}",
        "name": os.path.basename(path),
    }


def list_media(roots, progress=None):
    """返回可展示的文件记录。progress(path) 用于界面上的「正在扫描」。"""
    found = []
    seen = 0
    for root in roots or []:
        if not root or not os.path.isdir(root):
            continue
        for entry in utils.walk_files(root):
            seen += 1
            if progress and seen % 80 == 1:
                progress(entry.path)
            try:
                stat = entry.stat(follow_symlinks=False)
                size = stat.st_size
                mtime = stat.st_mtime
            except OSError:
                continue
            record = _entry_record(entry.path, size, mtime)
            if record:
                found.append(record)
    found.sort(key=lambda item: item["mtime"], reverse=True)
    return found


def aggregate(records):
    result = {key: {"size": 0, "count": 0} for key, _ in KINDS}
    for record in records or []:
        kind = record.get("kind")
        if kind not in result:
            continue
        result[kind]["size"] += int(record.get("size") or 0)
        result[kind]["count"] += 1
    return result


def scan(roots):
    return aggregate(list_media(roots))


def _within_roots(path, roots):
    return utils.is_within(path, [root for root in roots or [] if root])


def clean(roots, kinds, paths=None, remover=None):
    """清理选中的媒体。remover(paths) 负责实际删除，默认送进回收站。

    即使调用方传入了路径，也会再次分类：数据库、配置、密码库不会被删。
    """
    import recyclebin
    import safepath

    kinds = set(kinds or [])
    freed = 0
    deleted = 0
    chosen = []
    if paths is None:
        for record in list_media(roots):
            if record["kind"] in kinds and _within_roots(record["path"], roots):
                chosen.append(record)
    else:
        for path in paths:
            if not path or not _within_roots(path, roots):
                continue
            if safepath.is_password_store(path) or safepath.is_protected_path(path):
                continue
            kind, _sub = classify(path)
            if kind not in kinds:
                continue
            try:
                size = os.path.getsize(path)
                mtime = os.path.getmtime(path)
            except OSError:
                continue
            record = _entry_record(path, size, mtime)
            if record and record["kind"] in kinds:
                chosen.append(record)
    if not chosen:
        return 0, 0
    remove = remover or recyclebin.send_to_recycle
    batch = [record["path"] for record in chosen]
    failed = set(remove(batch) or [])
    for record in chosen:
        if record["path"] in failed:
            continue
        if not os.path.exists(record["path"]):
            freed += record["size"]
            deleted += 1
    return freed, deleted
