"""Windows 资源管理器拖放。其他系统上静默跳过，添加按钮仍然可用。"""
import os


def enable_file_drop(widget, callback) -> bool:
    if os.name != "nt":
        return False
    try:
        import ctypes
        from ctypes import wintypes
    except Exception:
        return False
    try:
        widget.update_idletasks()
        hwnd = widget.winfo_id()
        user32 = ctypes.windll.user32
        shell32 = ctypes.windll.shell32
        parent = user32.GetParent(hwnd) or hwnd
        target = parent
        GWL_WNDPROC = -4
        WM_DROPFILES = 0x0233
        if ctypes.sizeof(ctypes.c_void_p) == 8:
            get_long = user32.GetWindowLongPtrW
            set_long = user32.SetWindowLongPtrW
            get_long.restype = ctypes.c_void_p
            set_long.restype = ctypes.c_void_p
        else:
            get_long = user32.GetWindowLongW
            set_long = user32.SetWindowLongW
        old = get_long(target, GWL_WNDPROC)
        if not old:
            return False

        prototype = ctypes.WINFUNCTYPE(
            ctypes.c_long, wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM)

        @prototype
        def proc(handle, msg, wparam, lparam):
            if msg == WM_DROPFILES:
                count = shell32.DragQueryFileW(wparam, 0xFFFFFFFF, None, 0)
                paths = []
                for index in range(int(count)):
                    length = shell32.DragQueryFileW(wparam, index, None, 0)
                    buf = ctypes.create_unicode_buffer(int(length) + 1)
                    shell32.DragQueryFileW(wparam, index, buf, int(length) + 1)
                    if buf.value:
                        paths.append(buf.value)
                shell32.DragFinish(wparam)
                if paths:
                    widget.after(0, lambda items=list(paths): callback(items))
                return 0
            return user32.CallWindowProcW(old, handle, msg, wparam, lparam)

        widget._drop_proc = proc
        set_long(target, GWL_WNDPROC, proc)
        shell32.DragAcceptFiles(target, True)
        return True
    except Exception:
        return False
