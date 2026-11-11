"""文件粉碎：覆盖写后删除，以及删除后清理空目录。

系统路径直接拒绝。粉碎本身不可恢复，不走回收站。
打开文件时不跟随链接，只覆盖事先 lstat 到的那个普通文件。
"""
import os
import stat

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


def _open_regular_nofollow(path):
    """不跟随链接地打开普通文件，并核对设备号和 inode 与打开前的 lstat 一致。"""
    if os.name == "nt":
        return _open_regular_windows(path)
    try:
        info = os.lstat(path)
    except OSError as exc:
        raise ShredError(f"文件不存在：{path}") from exc
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise ShredError(f"不能粉碎符号链接或目录联接：{path}")
    if safepath.is_reparse_point(path):
        raise ShredError(f"不能粉碎符号链接或目录联接：{path}")
    flags = os.O_RDWR
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    try:
        fd = os.open(path, flags)
    except OSError as exc:
        raise ShredError(f"无法安全打开文件：{path}") from exc
    try:
        current = os.fstat(fd)
        attributes = getattr(current, "st_file_attributes", 0) or 0
        if (not stat.S_ISREG(current.st_mode)
                or attributes & safepath.FILE_ATTRIBUTE_REPARSE_POINT
                or current.st_dev != info.st_dev
                or current.st_ino != info.st_ino):
            raise ShredError(f"文件在打开前被替换：{path}")
        return os.fdopen(fd, "r+b")
    except Exception:
        os.close(fd)
        raise


def _open_regular_windows(path):
    import ctypes
    import msvcrt
    from ctypes import wintypes
    try:
        info = os.lstat(path)
    except OSError as exc:
        raise ShredError(f"文件不存在：{path}") from exc
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise ShredError(f"不能粉碎符号链接或目录联接：{path}")
    if getattr(info, "st_file_attributes", 0) & safepath.FILE_ATTRIBUTE_REPARSE_POINT:
        raise ShredError(f"不能粉碎符号链接或目录联接：{path}")
    generic_read = 0x80000000
    generic_write = 0x40000000
    share = 0x1 | 0x2 | 0x4
    open_existing = 3
    flag = 0x00200000  # FILE_FLAG_OPEN_REPARSE_POINT
    kernel = ctypes.windll.kernel32
    handle = kernel.CreateFileW(
        str(path), generic_read | generic_write, share, None,
        open_existing, flag, None)
    invalid = ctypes.c_void_p(-1).value
    if handle in (0, invalid, -1):
        raise ShredError(f"无法安全打开文件：{path}")

    class _ByHandle(ctypes.Structure):
        _fields_ = [
            ("dwFileAttributes", wintypes.DWORD),
            ("ftCreationTime", wintypes.FILETIME),
            ("ftLastAccessTime", wintypes.FILETIME),
            ("ftLastWriteTime", wintypes.FILETIME),
            ("dwVolumeSerialNumber", wintypes.DWORD),
            ("nFileSizeHigh", wintypes.DWORD),
            ("nFileSizeLow", wintypes.DWORD),
            ("nNumberOfLinks", wintypes.DWORD),
            ("nFileIndexHigh", wintypes.DWORD),
            ("nFileIndexLow", wintypes.DWORD),
        ]

    try:
        got = _ByHandle()
        if not kernel.GetFileInformationByHandle(handle, ctypes.byref(got)):
            raise ShredError(f"无法读取文件属性：{path}")
        file_id = (got.nFileIndexHigh << 32) | got.nFileIndexLow
        if (got.dwFileAttributes & safepath.FILE_ATTRIBUTE_REPARSE_POINT
                or got.dwVolumeSerialNumber != info.st_dev
                or file_id != info.st_ino):
            raise ShredError(f"文件在打开前被替换：{path}")
        fd = msvcrt.open_osfhandle(handle, getattr(os, "O_BINARY", 0))
    except Exception:
        kernel.CloseHandle(handle)
        raise
    try:
        current = os.fstat(fd)
        if (not stat.S_ISREG(current.st_mode)
                or current.st_dev != info.st_dev
                or current.st_ino != info.st_ino):
            raise ShredError(f"文件在打开前被替换：{path}")
        return os.fdopen(fd, "r+b")
    except Exception:
        os.close(fd)
        raise


def shred_file(path, passes=1):
    handle = _open_regular_nofollow(path)
    try:
        size = os.fstat(handle.fileno()).st_size
        for _ in range(max(1, passes)):
            handle.seek(0)
            left = size
            while left > 0:
                block = min(1 << 20, left)
                handle.write(os.urandom(block))
                left -= block
            handle.flush()
            os.fsync(handle.fileno())
    finally:
        handle.close()
    safepath.unlink_regular(path, [os.path.dirname(path) or "."])


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


def _shred_directory(path, passes=1):
    """粉碎目录中的普通文件；逐项返回真实失败，不跟随链接。"""
    success_paths = []
    failures = []
    for entry in utils.walk_files(path):
        try:
            assert_shreddable(entry.path)
            shred_file(entry.path, passes)
            success_paths.append(entry.path)
        except Exception as exc:
            failures.append({"path": entry.path, "status": "failed", "error": str(exc)})

    _remove_empty_dirs(path)
    root_removed = False
    if not failures:
        try:
            os.rmdir(path)
            root_removed = True
        except OSError as exc:
            failures.append({"path": path, "status": "failed", "error": str(exc)})
    return success_paths, failures, root_removed


def run_batch(paths, shred_fn=None):
    """粉碎路径并保留目录内逐文件结果，失败不会被吞掉或算作成功。"""
    records = []
    for path in paths or []:
        if shred_fn is None and os.path.isdir(path) and not os.path.islink(path):
            try:
                assert_shreddable(path)
                success_paths, failures, root_removed = _shred_directory(path)
                status = "ok" if not failures else ("partial" if success_paths else "failed")
                records.append({
                    "path": path,
                    "status": status,
                    "files_ok": len(success_paths),
                    "files_failed": len(failures),
                    "success_paths": success_paths,
                    "failures": failures,
                    "finished": root_removed and not failures,
                })
            except Exception as exc:
                records.append({
                    "path": path, "status": "failed", "files_ok": 0,
                    "files_failed": 1, "success_paths": [],
                    "failures": [{"path": path, "status": "failed", "error": str(exc)}],
                    "finished": False,
                })
            continue

        try:
            if shred_fn is None:
                result = shred_path(path)
            else:
                result = shred_fn(path)
            count = result if isinstance(result, int) else 1
            records.append({
                "path": path, "status": "ok", "files_ok": max(count, 1),
                "files_failed": 0, "success_paths": [path], "failures": [],
                "finished": True,
            })
        except Exception as exc:
            failure = {"path": path, "status": "failed", "error": str(exc)}
            records.append({
                "path": path, "status": "failed", "files_ok": 0,
                "files_failed": 1, "success_paths": [], "failures": [failure],
                "finished": False, "error": str(exc),
            })
    return records


def shred_path(path, passes=1, log=lambda _message: None):
    assert_shreddable(path)
    if os.path.isfile(path):
        shred_file(path, passes)
        log(path)
        return 1
    if os.path.isdir(path):
        success_paths, failures, _root_removed = _shred_directory(path, passes)
        for done_path in success_paths:
            log(done_path)
        if failures:
            detail = "; ".join(
                f"{item.get('path')}: {item.get('error', '粉碎失败')}"
                for item in failures[:5])
            raise ShredError(f"目录内有 {len(failures)} 项未能粉碎：{detail}")
        return len(success_paths)
    return 0
