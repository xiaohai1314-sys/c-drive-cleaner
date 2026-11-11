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


def find_duplicates(roots, min_size=1 << 20, progress=None, cancel=None):
    """返回重复组列表：{hash, size, files:[path,...]}。"""
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
                    groups.append({"hash": digest, "size": size, "files": sorted(same)})
        if progress:
            progress(len(groups))

    groups.sort(key=lambda item: item["size"] * len(item["files"]), reverse=True)
    return groups
