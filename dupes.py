"""重复文件查找：按大小分组 → 采样哈希 → 全文件哈希。"""
import hashlib
import os
import stat

import safepath
import utils


def _file_hash(path, chunk=1024 * 1024):
    digest = hashlib.sha1()
    try:
        with open(path, "rb") as handle:
            while True:
                block = handle.read(chunk)
                if not block:
                    break
                digest.update(block)
    except OSError:
        return None
    return digest.hexdigest()


def _sample_hash(path, size, sample=8192):
    digest = hashlib.sha1()
    try:
        with open(path, "rb") as handle:
            digest.update(handle.read(sample))
            if size > sample * 2:
                handle.seek(-sample, os.SEEK_END)
                digest.update(handle.read(sample))
    except OSError:
        return None
    return digest.hexdigest()


def _stat_snapshot(info):
    return {
        "size": info.st_size,
        "mtime_ns": getattr(info, "st_mtime_ns", int(info.st_mtime * 1_000_000_000)),
        "device": info.st_dev,
        "inode": info.st_ino,
    }


def _same_snapshot(left, right):
    return all(left.get(key) == right.get(key)
               for key in ("size", "mtime_ns", "device", "inode"))


def _hash_regular_file(path):
    """Hash an unchanged regular-file handle without following symlinks/reparse points."""
    if not path or safepath.is_reparse_point(path):
        return None, None
    try:
        before = os.lstat(path)
        if not stat.S_ISREG(before.st_mode):
            return None, None
        flags = os.O_RDONLY
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        if hasattr(os, "O_BINARY"):
            flags |= os.O_BINARY
        fd = os.open(path, flags)
    except OSError:
        return None, None
    digest = hashlib.sha1()
    try:
        opened = os.fstat(fd)
        before_snapshot = _stat_snapshot(before)
        if not stat.S_ISREG(opened.st_mode) or not _same_snapshot(
                before_snapshot, _stat_snapshot(opened)):
            return None, None
        with os.fdopen(fd, "rb", closefd=False) as handle:
            while True:
                block = handle.read(1024 * 1024)
                if not block:
                    break
                digest.update(block)
        after_handle = os.fstat(fd)
        after_path = os.lstat(path)
        after_snapshot = _stat_snapshot(after_path)
        if (not stat.S_ISREG(after_handle.st_mode)
                or not _same_snapshot(before_snapshot, _stat_snapshot(after_handle))
                or not _same_snapshot(before_snapshot, after_snapshot)
                or safepath.is_reparse_point(path)):
            return None, None
        return digest.hexdigest(), after_snapshot
    except OSError:
        return None, None
    finally:
        try:
            os.close(fd)
        except OSError:
            pass


def verify_duplicate_candidate(path, group):
    """Return true only if a selected path still matches its scanned duplicate snapshot."""
    if not path or safepath.is_protected_path(path):
        return False
    expected_hash = (group or {}).get("hash")
    try:
        expected_size = int((group or {}).get("size"))
    except (TypeError, ValueError):
        return False
    if not expected_hash or not os.path.lexists(path):
        return False
    digest, current = _hash_regular_file(path)
    if digest is None or digest != expected_hash or current.get("size") != expected_size:
        return False
    expected = ((group or {}).get("identities") or {}).get(path)
    if expected and not _same_snapshot(expected, current):
        return False
    return True


KEEP_RULES = (
    ("newest", "保留最新修改"),
    ("oldest", "保留最早修改"),
    ("shortest", "保留路径最短"),
)
_keep_rule = "newest"
# 用户手动指定的保留项：组哈希 → 路径。切换规则时清空；路径已不在组里时自动失效。
_keeper_overrides = {}


def set_keep_rule(rule) -> None:
    """切换每组保留哪一份。未知规则回到默认的「最新修改」。手动指定的保留项一并清掉。"""
    global _keep_rule
    _keep_rule = rule if rule in {key for key, _label in KEEP_RULES} else "newest"
    _keeper_overrides.clear()


def set_keeper(group, path) -> bool:
    """手动指定这一组保留哪份。路径不在组里时不改，返回 False。"""
    key = (group or {}).get("hash")
    if not key or path not in ((group or {}).get("files") or []):
        return False
    _keeper_overrides[key] = path
    return True


def clear_keepers() -> None:
    _keeper_overrides.clear()


def keep_rule() -> str:
    return _keep_rule


def pick_keeper(group, rule=None) -> str:
    """每组留下的那一份。

    newest：修改时间最新，其次路径更短；oldest：修改时间最早，其次路径更短；
    shortest：路径最短，其次修改时间最新。最后都按路径排序，结果稳定。
    """
    files = list(group.get("files") or [])
    if not files:
        return ""
    if rule is None:
        chosen = _keeper_overrides.get(group.get("hash"))
        if chosen in files:
            return chosen
    mtimes = group.get("mtimes") or {}
    rule = rule or _keep_rule
    if rule == "oldest":
        return min(files, key=lambda path: (mtimes.get(path, 0), len(path), path))
    if rule == "shortest":
        return min(files, key=lambda path: (len(path), -mtimes.get(path, 0), path))
    return max(files, key=lambda path: (mtimes.get(path, 0), -len(path), path))


def auto_select(groups) -> set:
    """默认勾选：每组除保留份以外的全部副本。"""
    chosen = set()
    for group in groups or []:
        files = list(group.get("files") or [])
        if len(files) < 2:
            continue
        keeper = pick_keeper(group)
        for path in files:
            if path != keeper:
                chosen.add(path)
    return chosen


def paths_for_cleanup(groups, selected, kinds=None) -> list:
    """只删除当前类型筛选里看得见、并且勾选了的副本。隐藏分组不参与删除。"""
    visible = []
    allowed = set(kinds) if kinds is not None else None
    for group in groups or []:
        files = list(group.get("files") or [])
        if not files:
            continue
        if allowed is not None:
            import filetypes
            if filetypes.kind_of(files[0]) not in allowed:
                continue
        visible.append(group)
    return paths_to_delete(visible, selected)


def paths_to_delete(groups, selected) -> list:
    """返回勾选的非保留副本；保留项无论 UI 状态如何都不会进入删除计划。"""
    selected = set(selected or [])
    doomed = []
    seen = set()
    for group in groups or []:
        files = list(group.get("files") or [])
        if not files:
            continue
        keeper = pick_keeper(group)
        for path in files:
            if path in selected and path != keeper and path not in seen:
                seen.add(path)
                doomed.append(path)
    return doomed


def _name_key(path):
    name = os.path.basename(path.replace("\\", os.sep)).casefold()
    normalized = os.path.normcase(path).casefold()
    return name, normalized, path


def sort_duplicate_groups(groups, key="size", desc=True):
    """Sort visible groups, and sort member paths when the name column is active."""
    ordered = []
    for source in groups or []:
        group = dict(source)
        files = list(group.get("files") or [])
        if key == "name":
            files.sort(key=_name_key, reverse=bool(desc))
        group["files"] = files
        if key == "mtime":
            value = max((group.get("mtimes") or {}).values() or [0])
            sort_key = (value, tuple(_name_key(path) for path in files))
        elif key == "name":
            sort_key = _name_key(files[0]) if files else ("", "", "")
        else:
            sort_key = (group.get("size") or 0, tuple(_name_key(path) for path in files))
        ordered.append((sort_key, group))
    ordered.sort(key=lambda item: item[0], reverse=bool(desc))
    return [group for _sort_key_value, group in ordered]


def find_duplicates(roots, min_size=1 << 20, progress=None, cancel=None):
    """返回重复组列表：{hash, size, files, mtimes, identities}。"""
    import fastscan
    by_size = {}
    for root in roots:
        if not os.path.isdir(root):
            continue
        for path, size in fastscan.iter_files(root, min_size=min_size):
            if cancel and cancel():
                return []
            by_size.setdefault(size, []).append(path)

    groups = []
    for size, paths in by_size.items():
        if len(paths) < 2:
            continue
        sampled = {}
        for path in paths:
            digest = _sample_hash(path, size)
            if digest:
                sampled.setdefault(digest, []).append(path)
        for candidates in sampled.values():
            if len(candidates) < 2:
                continue
            exact = {}
            for path in candidates:
                digest = _file_hash(path)
                if digest:
                    exact.setdefault(digest, []).append(path)
            for digest, same in exact.items():
                if len(same) > 1:
                    ordered = sorted(same)
                    mtimes = {}
                    identities = {}
                    for path in ordered:
                        try:
                            info = os.lstat(path)
                            snapshot = _stat_snapshot(info)
                            mtimes[path] = info.st_mtime
                            identities[path] = snapshot
                        except OSError:
                            mtimes[path] = 0
                    groups.append({
                        "hash": digest,
                        "size": size,
                        "files": ordered,
                        "mtimes": mtimes,
                        "identities": identities,
                    })
        if progress:
            progress(len(groups))

    groups.sort(key=lambda item: item["size"] * len(item["files"]), reverse=True)
    return groups
