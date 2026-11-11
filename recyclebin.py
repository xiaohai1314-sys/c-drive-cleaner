"""把文件送进回收站。失败时不退回到永久删除。"""
import os
import sys


def describe() -> str:
    return "移到回收站（可从回收站恢复）"


def send_to_recycle(paths) -> list:
    """把路径送进回收站。返回未能处理的路径。非 Windows 直接全部算失败。"""
    paths = [os.path.abspath(path) for path in paths if path]
    if not paths:
        return []
    if os.name != "nt":
        return list(paths)
    failed = []
    chunk = []
    for path in paths:
        chunk.append(path)
        if len(chunk) >= 40:
            failed.extend(_send_chunk(chunk))
            chunk = []
    if chunk:
        failed.extend(_send_chunk(chunk))
    return failed


def _send_chunk(paths) -> list:
    import ctypes
    from ctypes import wintypes

    class SHFILEOPSTRUCTW(ctypes.Structure):
        _fields_ = [
            ("hwnd", wintypes.HWND),
            ("wFunc", ctypes.c_uint),
            ("pFrom", ctypes.c_wchar_p),
            ("pTo", ctypes.c_wchar_p),
            ("fFlags", ctypes.c_ushort),
            ("fAnyOperationsAborted", wintypes.BOOL),
            ("hNameMappings", ctypes.c_void_p),
            ("lpszProgressTitle", ctypes.c_wchar_p),
        ]

    buffer = "\0".join(paths) + "\0\0"
    op = SHFILEOPSTRUCTW()
    op.hwnd = None
    op.wFunc = 3  # FO_DELETE
    op.pFrom = buffer
    op.pTo = None
    op.fFlags = 0x40 | 0x10 | 0x400  # ALLOWUNDO | NOCONFIRMATION | NOERRORUI
    op.fAnyOperationsAborted = False
    op.hNameMappings = None
    op.lpszProgressTitle = None
    try:
        code = ctypes.windll.shell32.SHFileOperationW(ctypes.byref(op))
    except Exception:
        return list(paths)
    if code == 0 and not op.fAnyOperationsAborted:
        return []
    return list(paths)


def preview_lines(paths, limit=12) -> list:
    shown = list(paths[:limit])
    lines = list(shown)
    extra = len(paths) - len(shown)
    if extra > 0:
        lines.append(f"…… 另有 {extra} 个")
    return lines
