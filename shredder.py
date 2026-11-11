"""文件粉碎：覆盖写后删除，以及删除后清理空目录。

系统路径直接拒绝。粉碎本身不可恢复，不走回收站。
"""
import os

import safepath
import utils


class ShredError(Exception):
    pass


def assert_shreddable(path: str):
    if not path or not os.path.lexists(path):
        raise ShredError("文件不存在")
    if safepath.is_reparse_point(path):
        raise ShredError(f"不能粉碎符号链接或目录联接：{path}")
    if safepath.is_protected_path(path):
        raise ShredError(f"为保护系统，拒绝粉碎：{path}")


def shred_file(path, passes=1):
    if safepath.is_reparse_point(path):
        raise ShredError(f"不能粉碎符号链接或目录联接：{path}")
    size = os.path.getsize(path)
    with open(path, "r+b") as handle:
        for _ in range(max(1, passes)):
            handle.seek(0)
            left = size
            while left > 0:
                block = min(1 << 20, left)
                handle.write(os.urandom(block))
                left -= block
            handle.flush()
            os.fsync(handle.fileno())
    os.remove(path)


def _remove_empty_dirs(root):
    stack = [root]
    dirs = []
    while stack:
        current = stack.pop()
        try:
            with os.scandir(current) as it:
                for entry in it:
                    try:
                        if entry.is_dir(follow_symlinks=False):
                            if utils.is_link_like(entry):
                                try:
                                    os.rmdir(entry.path)
                                except OSError:
                                    pass
                            else:
                                dirs.append(entry.path)
                                stack.append(entry.path)
                    except OSError:
                        continue
        except OSError:
            continue
    for path in sorted(dirs, key=len, reverse=True):
        try:
            os.rmdir(path)
        except OSError:
            pass


def shred_path(path, passes=1, log=lambda _message: None):
    assert_shreddable(path)
    if os.path.isfile(path):
        shred_file(path, passes)
        log(path)
        return 1
    if os.path.isdir(path):
        count = 0
        for entry in utils.walk_files(path):
            try:
                shred_file(entry.path, passes)
                count += 1
            except OSError:
                pass
        _remove_empty_dirs(path)
        try:
            os.rmdir(path)
        except OSError:
            pass
        return count
    return 0
