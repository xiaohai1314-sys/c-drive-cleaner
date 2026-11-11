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


def pick_keeper(group) -> str:
    """每组留下的那一份：优先修改时间最新，其次路径更短，最后按路径排序。"""
    files = list(group.get("files") or [])
    if not files:
        return ""
    mtimes = group.get("mtimes") or {}

    def sort_key(path):
        return (mtimes.get(path, 0), -len(path), path)

    return max(files, key=sort_key)


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
