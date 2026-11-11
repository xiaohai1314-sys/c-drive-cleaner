"""文件类型色块，以及在 Windows 上尽量读取真实文件图标。"""
import os

_COLORS = {
    "video": (142, 68, 173),
    "music": (22, 160, 133),
    "document": (47, 128, 237),
    "image": (230, 126, 34),
    "archive": (211, 84, 0),
    "installer": (192, 57, 43),
    "other": (127, 140, 141),
    "app": (47, 128, 237),
}


def badge(kind, size=16):
    import tkinter as tk
    red, green, blue = _COLORS.get(kind, _COLORS["other"])
    header = f"P6 {size} {size} 255\n".encode("ascii")
    pixel = bytes((red, green, blue))
    return tk.PhotoImage(data=header + pixel * (size * size))


def file_icon(path, size=16):
    """优先用系统图标，失败时退回类型色块。调用前需要已经创建 Tk。"""
    photo = None
    if os.name == "nt" and path and os.path.exists(path):
        try:
            photo = _shell_icon(path, size)
        except Exception:
            photo = None
    if photo is not None:
        return photo
    import filetypes
    kind = filetypes.kind_of(path or "")
    return badge(kind, size)


def _shell_icon(path, size):
    import ctypes
    from ctypes import wintypes
    import tkinter as tk

    class SHFILEINFOW(ctypes.Structure):
        _fields_ = [
            ("hIcon", wintypes.HICON),
            ("iIcon", ctypes.c_int),
            ("dwAttributes", wintypes.DWORD),
            ("szDisplayName", wintypes.WCHAR * 260),
            ("szTypeName", wintypes.WCHAR * 80),
        ]

    info = SHFILEINFOW()
    flags = 0x100 | 0x1  # SHGFI_ICON | SHGFI_SMALLICON
    ok = ctypes.windll.shell32.SHGetFileInfoW(
        str(path), 0, ctypes.byref(info), ctypes.sizeof(info), flags)
    if not ok or not info.hIcon:
        return None
    try:
        return _hicon_to_photo(info.hIcon, size)
    finally:
        ctypes.windll.user32.DestroyIcon(info.hIcon)


def _hicon_to_photo(hicon, size):
    import ctypes
    from ctypes import wintypes
    import tkinter as tk

    class BITMAPINFOHEADER(ctypes.Structure):
        _fields_ = [
            ("biSize", wintypes.DWORD),
            ("biWidth", wintypes.LONG),
            ("biHeight", wintypes.LONG),
            ("biPlanes", wintypes.WORD),
            ("biBitCount", wintypes.WORD),
            ("biCompression", wintypes.DWORD),
            ("biSizeImage", wintypes.DWORD),
            ("biXPelsPerMeter", wintypes.LONG),
            ("biYPelsPerMeter", wintypes.LONG),
            ("biClrUsed", wintypes.DWORD),
            ("biClrImportant", wintypes.DWORD),
        ]

    class BITMAPINFO(ctypes.Structure):
        _fields_ = [("bmiHeader", BITMAPINFOHEADER), ("bmiColors", wintypes.DWORD * 3)]

    user32 = ctypes.windll.user32
    gdi32 = ctypes.windll.gdi32
    hdc = user32.GetDC(None)
    mem = gdi32.CreateCompatibleDC(hdc)
    bmi = BITMAPINFO()
    bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bmi.bmiHeader.biWidth = size
    bmi.bmiHeader.biHeight = -size
    bmi.bmiHeader.biPlanes = 1
    bmi.bmiHeader.biBitCount = 32
    bits = ctypes.c_void_p()
    hbmp = gdi32.CreateDIBSection(mem, ctypes.byref(bmi), 0, ctypes.byref(bits), None, 0)
    if not hbmp or not bits:
        gdi32.DeleteDC(mem)
        user32.ReleaseDC(None, hdc)
        return None
    old = gdi32.SelectObject(mem, hbmp)
    user32.DrawIconEx(mem, 0, 0, hicon, size, size, 0, None, 3)
    raw = bytearray()
    buf = (ctypes.c_ubyte * (size * size * 4)).from_address(bits.value)
    for index in range(0, size * size * 4, 4):
        blue, green, red = buf[index], buf[index + 1], buf[index + 2]
        raw.extend((red, green, blue))
    gdi32.SelectObject(mem, old)
    gdi32.DeleteObject(hbmp)
    gdi32.DeleteDC(mem)
    user32.ReleaseDC(None, hdc)
    header = f"P6 {size} {size} 255\n".encode("ascii")
    return tk.PhotoImage(data=header + bytes(raw))


# 清理卡片上能对应到安装程序的名字。找不到就继续用色块。
_APP_EXES = (
    ("chrome", ("Google/Chrome/Application/chrome.exe",)),
    ("edge", ("Microsoft/Edge/Application/msedge.exe",)),
    ("firefox", ("Mozilla Firefox/firefox.exe",)),
    ("微信", ("Tencent/WeChat/WeChat.exe", "Tencent/Weixin/Weixin.exe")),
    ("qq", ("Tencent/QQ/Bin/QQ.exe", "Tencent/QQNT/QQ.exe")),
)


def find_app_exe(label: str) -> str:
    if os.name != "nt" or not label:
        return ""
    text = label.lower()
    program = os.environ.get("ProgramFiles", r"C:\Program Files")
    program_x86 = os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")
    local = os.environ.get("LOCALAPPDATA", "")
    for key, relatives in _APP_EXES:
        if key not in text and key not in label:
            continue
        for relative in relatives:
            for base in (program, program_x86, local):
                if not base:
                    continue
                path = os.path.join(base, *relative.split("/"))
                if os.path.isfile(path):
                    return path
    return ""
