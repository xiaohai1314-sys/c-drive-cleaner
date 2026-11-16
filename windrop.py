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
            get_long.argtypes = [wintypes.HWND, ctypes.c_int]
            set_long.restype = ctypes.c_void_p
            set_long.argtypes = [wintypes.HWND, ctypes.c_int, ctypes.c_void_p]
        else:
            get_long = user32.GetWindowLongW
            set_long = user32.SetWindowLongW
            get_long.restype = ctypes.c_void_p
            get_long.argtypes = [wintypes.HWND, ctypes.c_int]
            set_long.restype = ctypes.c_void_p
            set_long.argtypes = [wintypes.HWND, ctypes.c_int, ctypes.c_void_p]
        old = get_long(target, GWL_WNDPROC)
        if not old:
            return False
        # 不声明 argtypes 时 ctypes 会把 64 位指针当 32 位整数转换，每条窗口消息都抛
        # OverflowError，界面因此持续卡顿。
        call_proc = user32.CallWindowProcW
        call_proc.restype = ctypes.c_ssize_t
        call_proc.argtypes = [ctypes.c_void_p, wintypes.HWND, wintypes.UINT,
                              wintypes.WPARAM, wintypes.LPARAM]
        drag_query = shell32.DragQueryFileW
        drag_query.restype = wintypes.UINT
        drag_query.argtypes = [ctypes.c_void_p, wintypes.UINT, wintypes.LPWSTR, wintypes.UINT]
        drag_finish = shell32.DragFinish
        drag_finish.restype = None
        drag_finish.argtypes = [ctypes.c_void_p]
        accept = shell32.DragAcceptFiles
        accept.restype = None
        accept.argtypes = [wintypes.HWND, wintypes.BOOL]

        prototype = ctypes.WINFUNCTYPE(
            ctypes.c_ssize_t, wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM)

        @prototype
        def proc(handle, msg, wparam, lparam):
            try:
                if msg == WM_DROPFILES:
                    count = drag_query(ctypes.c_void_p(wparam), 0xFFFFFFFF, None, 0)
                    paths = []
                    for index in range(int(count)):
                        length = drag_query(ctypes.c_void_p(wparam), index, None, 0)
                        buf = ctypes.create_unicode_buffer(int(length) + 1)
                        drag_query(ctypes.c_void_p(wparam), index, buf, int(length) + 1)
                        if buf.value:
                            paths.append(buf.value)
                    drag_finish(ctypes.c_void_p(wparam))
                    if paths:
                        widget.after(0, lambda items=list(paths): callback(items))
                    return 0
            except Exception:
                pass
            try:
                return int(call_proc(old, handle, msg, wparam, lparam) or 0)
            except Exception:
                return 0

        widget._drop_proc = proc
        set_long(target, GWL_WNDPROC, ctypes.cast(proc, ctypes.c_void_p))
        accept(target, True)
        return True
    except Exception:
        return False
