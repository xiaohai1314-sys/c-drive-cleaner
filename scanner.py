"""扫描：统计各清理项大小，以及目录/大文件分析。"""
import ctypes
import fnmatch
import os
import shutil

from utils import is_link_like, is_runtime_path, walk_files


class _SHQUERYRBINFO(ctypes.Structure):
    _fields_ = [
        ("cbSize", ctypes.c_ulong),
        ("i64Size", ctypes.c_longlong),
        ("i64NumItems", ctypes.c_longlong),
    ]


def match_name(name: str, patterns) -> bool:
    lowered = name.lower()
    return any(fnmatch.fnmatch(lowered, p.lower()) for p in patterns)


def recycle_bin_size(drives=None):
    total_size = 0
    total_items = 0
    for drive in (drives or ["C:\\"]):
        info = _SHQUERYRBINFO()
        info.cbSize = ctypes.sizeof(info)
        try:
            hr = ctypes.windll.shell32.SHQueryRecycleBinW(drive, ctypes.byref(info))
            if hr == 0:
                total_size += int(info.i64Size)
                total_items += int(info.i64NumItems)
        except Exception:
            continue
    return total_size, total_items


def scan_category(cat):
    """返回 (字节数, 文件数)。字节数 -1 表示无法预估。"""
    if cat.kind == "recyclebin":
        return recycle_bin_size(cat.paths)

    if cat.kind == "command":
        if cat.size_fn is None:
            return -1, 0
        try:
            return int(cat.size_fn() or 0), 1
        except Exception:
            return -1, 0

    total = 0
    count = 0
    for root in cat.paths:
        if not os.path.exists(root):
            continue
        if os.path.isfile(root):
            if not match_name(os.path.basename(root), cat.patterns):
                continue
            try:
                total += os.path.getsize(root)
                count += 1
            except OSError:
                pass
            continue
        for entry in walk_files(root):
            if not match_name(entry.name, cat.patterns):
                continue
            if is_runtime_path(entry.path):
                continue
            try:
                total += entry.stat(follow_symlinks=False).st_size
                count += 1
            except OSError:
                continue
    return total, count


def scan_disk(drive: str = "C:\\"):
    usage = shutil.disk_usage(drive)
    return usage.total, usage.used, usage.free


TOP_SKIP = {
    "$recycle.bin", "system volume information", "windows",
    "program files", "program files (x86)", "programdata",
    "recovery", "perflogs", "msocache", "config.msi", "$windows.~bt",
    "$windows.~ws", "windows.old",
}


def scan_top_dirs(root: str = "C:\\", limit: int = 40, progress=None):
    """统计一级目录占用，按大小降序。"""
    rows = []
    try:
        with os.scandir(root) as it:
            entries = [e for e in it
                       if e.is_dir(follow_symlinks=False) and not is_link_like(e)]
    except OSError:
        return rows

    total = len(entries)
    for index, entry in enumerate(entries):
        size = 0
        for file_entry in walk_files(entry.path):
            try:
                size += file_entry.stat(follow_symlinks=False).st_size
            except OSError:
                pass
        rows.append((entry.path, size))
        if progress:
            progress(index + 1, total, entry.path)

    rows.sort(key=lambda item: item[1], reverse=True)
    return rows[:limit]


def visible_large_paths(rows, kind="all", disk="全部"):
    """按类型页签和磁盘筛选得到当前看得见的路径。安装包归入「其他」。"""
    import filetypes
    visible = []
    for row in rows or []:
        path = row[0]
        file_kind = filetypes.kind_of(path)
        if file_kind == "installer":
            file_kind = "other"
        if kind and kind != "all" and file_kind != kind:
            continue
        text = str(path)
        drive = text[:2] if len(text) >= 2 and text[1] == ":" else ""
        if disk and disk != "全部" and drive.upper() != str(disk).upper():
            continue
        visible.append(path)
    return visible


def scan_large_files(root: str = "C:\\", min_size: int = 100 * 1024 * 1024,
                     limit: int = 100, progress=None):
    """查找大于 min_size 的文件，跳过系统目录，按大小降序。"""
    import fastscan
    files = fastscan.iter_files(root, skip_top=TOP_SKIP, min_size=min_size)
    if progress:
        progress(len(files), len(files), root)
    files.sort(key=lambda item: item[1], reverse=True)
    if limit is None:
        return files
    return files[:limit]
