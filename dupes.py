"""重复文件查找：按大小分组 → 采样哈希 → 全文件哈希。"""
import hashlib
import os

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


KEEP_RULES = (
    ("newest", "保留最新修改"),
    ("oldest", "保留最早修改"),
    ("shortest", "保留路径最短"),
)
_keep_rule = "newest"


def set_keep_rule(rule) -> None:
    """切换每组保留哪一份。未知规则回到默认的「最新修改」。"""
    global _keep_rule
    _keep_rule = rule if rule in {key for key, _label in KEEP_RULES} else "newest"


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
    """真正可删的路径。每组至少留一份，即使用户把整组都勾上。"""
    selected = set(selected or [])
    doomed = []
    seen = set()
    for group in groups or []:
        files = list(group.get("files") or [])
        if not files:
            continue
        chosen = [path for path in files if path in selected]
        if len(chosen) >= len(files):
            keeper = pick_keeper(group)
            chosen = [path for path in chosen if path != keeper]
        for path in chosen:
            if path not in seen:
                seen.add(path)
                doomed.append(path)
    return doomed


def find_duplicates(roots, min_size=1 << 20, progress=None, cancel=None):
    """返回重复组列表：{hash, size, files:[path,...], mtimes:{path:mtime}}。"""
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
                    for path in ordered:
                        try:
                            mtimes[path] = os.path.getmtime(path)
                        except OSError:
                            mtimes[path] = 0
                    groups.append({
                        "hash": digest,
                        "size": size,
                        "files": ordered,
                        "mtimes": mtimes,
                    })
        if progress:
            progress(len(groups))

    groups.sort(key=lambda item: item["size"] * len(item["files"]), reverse=True)
    return groups
