"""文件粉碎：覆盖写后删除，以及删除后清理空目录。"""
import os

import utils


def shred_file(path, passes=1):
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
