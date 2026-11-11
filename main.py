"""C 盘清理工具 —— 图形界面。窗口标题使用中性名称「磁盘清理助手」。"""
import ctypes
import os
import queue
import subprocess
import sys
import threading
import time
import tkinter as tk
from tkinter import filedialog, messagebox, ttk

import categories
import cleaner
import dupes
import filetypes
import fragviz
import history
import iconsutil
import imclean
import migrate
import privacy
import recyclebin
import registry
import ruleupdate
import safepath
import scanner
import shredder
import utils
import windrop

if getattr(sys, "frozen", False):
    APP_DIR = os.path.dirname(os.path.abspath(sys.executable))
else:
    APP_DIR = os.path.dirname(os.path.abspath(__file__))
LOG_DIR = os.path.join(APP_DIR, "logs")

BG = "#f2f4f7"
WHITE = "#ffffff"
GREEN = "#2fb56b"
GREEN_DARK = "#26a35f"
RED = "#e53935"
ORANGE = "#f5a623"
BLUE = "#2f80ed"
BLUE_BG = "#e8f2fd"
TEXT = "#1f2937"
MUTED = "#8a94a6"
BORDER = "#e8ebf0"
ROW_ALT = "#eaf4fe"
SIDEBAR_BG = "#f3f6fb"
SIDEBAR_W = 196
APP_NAME = "磁盘清理助手"

SIDEBAR = [
    ("clean", "C盘瘦身", "C", "#2f80ed"),
    ("frag", "碎片清理", "碎", "#16a085"),
    ("privacy", "隐私清理", "隐", "#8e44ad"),
    ("big", "大文件专清", "大", "#e67e22"),
    ("dup", "重复文件专清", "重", "#4b6cb7"),
    ("wechat", "微信专清", "微", "#07c160"),
    ("qq", "QQ专清", "Q", "#12b7f5"),
    ("shred", "文件粉碎", "粉", "#e74c3c"),
]

BIG_KINDS = ("all", "video", "music", "document", "image", "archive", "other")
DUP_KINDS = ("video", "music", "document", "image", "archive", "installer", "other")

ICON_COLORS = ["#2f80ed", "#16a085", "#8e44ad", "#e67e22", "#4b6cb7",
               "#07c160", "#12b7f5", "#e74c3c", "#d35400", "#2980b9",
               "#c0392b", "#27ae60"]


def _round_rect(canvas, x1, y1, x2, y2, r, **kwargs):
    points = [x1 + r, y1, x1 + r, y1, x2 - r, y1, x2 - r, y1, x2, y1,
              x2, y1 + r, x2, y1 + r, x2, y2 - r, x2, y2 - r, x2, y2,
              x2 - r, y2, x2 - r, y2, x1 + r, y2, x1 + r, y2, x1, y2,
              x1, y2 - r, x1, y2 - r, x1, y1 + r, x1, y1 + r, x1, y1]
    return canvas.create_polygon(points, smooth=True, **kwargs)


class GreenButton(tk.Canvas):
    def __init__(self, master, text, command, width=150, height=46):
        super().__init__(master, width=width, height=height,
                         bg=master.cget("bg"), highlightthickness=0, cursor="hand2")
        self._text = text
        self._command = command
        self._btn_w = width
        self._btn_h = height
        self._draw(GREEN)
        self.bind("<Button-1>", lambda _e: self._command())
        self.bind("<Enter>", lambda _e: self._draw(GREEN_DARK))
        self.bind("<Leave>", lambda _e: self._draw(GREEN))

    def _draw(self, color):
        self.delete("all")
        _round_rect(self, 1, 1, self._btn_w - 1, self._btn_h - 1, 10, fill=color, outline="")
        self.create_text(self._btn_w // 2, self._btn_h // 2, text=self._text, fill="white",
                         font=("Microsoft YaHei UI", 12, "bold"))


class IconTile(tk.Canvas):
    def __init__(self, master, color, symbol, size=34):
        super().__init__(master, width=size, height=size,
                         bg=master.cget("bg"), highlightthickness=0)
        _round_rect(self, 1, 1, size - 1, size - 1, 8, fill=color, outline="")
        self.create_text(size // 2, size // 2, text=symbol, fill="white",
                         font=("Microsoft YaHei UI", 11, "bold"))


class Pill(tk.Canvas):
    def __init__(self, master, text, bg, fg="white", height=20):
        width = max(34, int(len(text) * 8.5) + 16)
        super().__init__(master, width=width, height=height,
                         bg=master.cget("bg"), highlightthickness=0)
        _round_rect(self, 0, 0, width, height, height // 2, fill=bg, outline="")
        self.create_text(width // 2, height // 2, text=text, fill=fg,
                         font=("Microsoft YaHei UI", 9, "bold"))


class SidebarItem(tk.Frame):
    def __init__(self, master, key, label, symbol, color, command):
        super().__init__(master, bg=SIDEBAR_BG, cursor="hand2")
        self.key = key
        self._color = color
        self._symbol = symbol
        self._command = command
        self._selected = False
        self._inner = tk.Frame(self, bg=SIDEBAR_BG)
        self._inner.pack(fill="x", padx=8, pady=3)
        self.icon = tk.Canvas(self._inner, width=26, height=26, bg=SIDEBAR_BG,
                              highlightthickness=0)
        self.icon.pack(side="left", padx=(4, 8))
        self._draw_icon(color)
        self.label = tk.Label(self._inner, text=label, bg=SIDEBAR_BG, fg=TEXT,
                              font=("Microsoft YaHei UI", 10))
        self.label.pack(side="left")
        self.size_label = tk.Label(self._inner, text="", bg=SIDEBAR_BG, fg=ORANGE,
                                   font=("Microsoft YaHei UI", 8))
        self.size_label.pack(side="left", padx=(4, 0))
        for widget in (self, self._inner, self.icon, self.label, self.size_label):
            widget.bind("<Button-1>", lambda _e: self._command(key))

    def _draw_icon(self, color):
        canvas = self.icon
        canvas.delete("all")
        key = self.key
        if key == "clean":
            canvas.create_oval(4, 4, 20, 20, outline=color, width=2)
            canvas.create_line(16, 16, 22, 22, fill=color, width=2)
        elif key == "frag":
            for index, height in enumerate((7, 12, 9, 16, 11)):
                canvas.create_rectangle(3 + index * 4, 22 - height, 6 + index * 4, 22,
                                        fill=color, outline="")
        elif key == "privacy":
            canvas.create_oval(5, 4, 21, 20, outline=color, width=2)
            canvas.create_rectangle(11, 12, 15, 20, fill=color, outline="")
        elif key == "big":
            canvas.create_rectangle(4, 6, 22, 20, outline=color, width=2)
            canvas.create_line(4, 11, 22, 11, fill=color)
        elif key == "dup":
            canvas.create_rectangle(4, 6, 16, 18, outline=color, width=2)
            canvas.create_rectangle(10, 10, 22, 22, outline=color, width=2)
        elif key == "wechat":
            canvas.create_oval(3, 5, 23, 21, outline=color, width=2)
        elif key == "qq":
            canvas.create_oval(4, 4, 22, 22, outline=color, width=2)
            canvas.create_text(13, 13, text="Q", fill=color, font=("Arial", 9, "bold"))
        else:
            canvas.create_polygon(13, 3, 22, 20, 4, 20, outline=color, fill="", width=2)

    def set_size(self, text):
        self.size_label.configure(text=text)

    def set_selected(self, selected):
        self._selected = selected
        bg = WHITE if selected else SIDEBAR_BG
        fg = BLUE if selected else TEXT
        self.configure(bg=bg)
        self._inner.configure(bg=bg)
        self.icon.configure(bg=bg)
        self.label.configure(bg=bg, fg=fg)
        self.size_label.configure(bg=bg, fg=ORANGE)


class App(tk.Tk):
    def __init__(self):
        super().__init__()
        self.title(APP_NAME)
        self.geometry("1080x720")
        self.minsize(980, 660)
        self.configure(bg=BG)
        self.option_add("*Font", ("Microsoft YaHei UI", 10))

        self.categories = categories.get_categories()
        self.selected = {c.key for c in self.categories
                         if c.tier == categories.CONSERVATIVE}
        self.results = {}
        self.queue = queue.Queue()
        self.worker = None
        self.log_file = None
        self.clean_freed = 0
        self.pages = {}
        self.sidebar_items = {}
        self.current_page = None
        self.big_rows = []
        self.dup_groups = []
        self._dup_cancel = False
        self._hist_rows = []
        self.big_checked = set()
        self.dup_checked = set()
        self._card_vars = []
        self.mig_rows = []
        self.reg_selected = {k for k, _ in registry.GROUPS}
        self.reg_counts = {}
        self.im_scan = {"wechat": None, "qq": None}
        self.im_selected = {"wechat": {k for k, _ in imclean.KINDS},
                            "qq": {k for k, _ in imclean.KINDS}}
        self.im_bodies = {}
        self.im_subs = {}
        self.im_roots = {"wechat": imclean.wechat_roots, "qq": imclean.qq_roots}
        self._scanning = False
        self._cleaning = False
        self._dup_running = False
        self._big_running = False
        self._mig_running = False
        self._im_running = {"wechat": False, "qq": False}
        self._frag_running = False
        self.scan_complete = False
        self._screenshot = os.environ.get("CLEANER_SCREENSHOT") == "1"
        self.big_type = "all"
        self.big_sort_key = "size"
        self.big_sort_desc = True
        self.big_tab_labels = {}
        self.dup_filter = set(DUP_KINDS)
        self.dup_sort_key = "size"
        self.dup_sort_desc = True
        self.dup_tab_vars = {}
        self.dup_paths = {}
        self.im_files = {"wechat": [], "qq": []}
        self.im_checked_paths = {"wechat": set(), "qq": set()}
        self.im_tab = {"wechat": "video", "qq": "image"}
        self.im_limits = {}
        self.im_open = set()
        self.im_heads = {}
        self._photos = []
        self._icon_cache = {}
        self.frag_analysis = None
        self.frag_canvas = None
        self.shred_items = []
        self.privacy_records = []
        self.privacy_selected = set()
        self.privacy_mode = "progress"
        self.privacy_step = ""
        self.privacy_done_steps = 0
        self._privacy_running = False
        self.bigmove_rows = []
        self.bigmove_checked = set()

        self._build_style()
        self._build_ui()
        self._show_page("clean")
        self.after(100, self._poll)
        if self._screenshot:
            self.scan_complete = True
            self.after(250, self._load_screenshot_demo)
        else:
            self.after(200, self.start_scan)

    # ---------- 样式 ----------
    def _build_style(self):
        style = ttk.Style(self)
        style.theme_use("clam")
        style.configure("Treeview", rowheight=30, background=WHITE,
                        fieldbackground=WHITE, borderwidth=0,
                        font=("Microsoft YaHei UI", 9))
        style.configure("Treeview.Heading", font=("Microsoft YaHei UI", 10, "bold"),
                        background="#f5f7fa", relief="flat", padding=(6, 6))
        style.map("Treeview.Heading", background=[("active", "#eef1f7")])
        style.map("Treeview", background=[("selected", ROW_ALT)],
                  foreground=[("selected", TEXT)])
        style.configure("Vertical.TScrollbar", background="#d8dee8",
                        troughcolor=WHITE, borderwidth=0, arrowsize=12)

    # ---------- 布局 ----------
    def _build_ui(self):
        self.sidebar = tk.Frame(self, bg=SIDEBAR_BG, width=SIDEBAR_W)
        self.sidebar.pack(side="left", fill="y")
        self.sidebar.pack_propagate(False)
        self._build_sidebar()

        self.main = tk.Frame(self, bg=WHITE)
        self.main.pack(side="left", fill="both", expand=True)
        self._build_topbar()

        self.content = tk.Frame(self.main, bg=WHITE)
        self.content.pack(fill="both", expand=True)
        self._build_pages()

    def _build_sidebar(self):
        top = tk.Frame(self.sidebar, bg=SIDEBAR_BG)
        top.pack(fill="x", pady=(14, 10), padx=10)
        logo = tk.Canvas(top, width=30, height=30, bg=SIDEBAR_BG, highlightthickness=0)
        logo.pack(side="left")
        logo.create_oval(2, 2, 28, 28, fill="#2f80ed", outline="")
        logo.create_text(15, 15, text="C", fill="white", font=("Arial", 12, "bold"))
        tk.Label(top, text=APP_NAME, bg=SIDEBAR_BG, fg=TEXT,
                 font=("Microsoft YaHei UI", 11, "bold")).pack(side="left", padx=(8, 0))

        for key, label, symbol, color in SIDEBAR:
            item = SidebarItem(self.sidebar, key, label, symbol, color, self._show_page)
            item.pack(fill="x", pady=1)
            self.sidebar_items[key] = item

        bottom = tk.Frame(self.sidebar, bg=SIDEBAR_BG)
        bottom.pack(side="bottom", fill="x", pady=10, padx=14)
        history_link = tk.Label(bottom, text="清理历史", bg=SIDEBAR_BG, fg=BLUE, cursor="hand2",
                                font=("Microsoft YaHei UI", 9))
        history_link.pack()
        history_link.bind("<Button-1>", lambda _e: self._show_clean_history())
        rules = tk.Label(bottom, text="规则更新", bg=SIDEBAR_BG, fg=BLUE, cursor="hand2",
                         font=("Microsoft YaHei UI", 9))
        rules.pack(pady=(4, 0))
        rules.bind("<Button-1>", lambda _e: self._check_rules_update())

    def _build_topbar(self):
        bar = tk.Frame(self.main, bg=WHITE)
        bar.pack(fill="x", padx=14, pady=(10, 0))
        right = tk.Frame(bar, bg=WHITE)
        right.pack(side="right")
        for symbol, action in (("—", self.iconify), ("□", self._toggle_zoom), ("✕", self.destroy)):
            lbl = tk.Label(right, text=symbol, bg=WHITE, fg=MUTED, cursor="hand2",
                           font=("Microsoft YaHei UI", 10), padx=6)
            lbl.pack(side="left")
            lbl.bind("<Button-1>", lambda _e, a=action: a())

    def _toggle_zoom(self):
        self.state("zoomed" if self.state() != "zoomed" else "normal")

    # ---------- 页面容器 ----------
    def _build_pages(self):
        builders = {
            "clean": self._page_clean,
            "frag": self._page_frag,
            "privacy": self._page_privacy,
            "big": self._page_big,
            "dup": self._page_dup,
            "wechat": self._page_wechat,
            "qq": self._page_qq,
            "shred": self._page_shred,
            "migrate": self._page_migrate,
            "bigmove": self._page_bigmove,
        }
        for key, builder in builders.items():
            frame = tk.Frame(self.content, bg=WHITE)
            builder(frame)
            self.pages[key] = frame

    def _show_page(self, key):
        for name, frame in self.pages.items():
            if name == key:
                frame.pack(fill="both", expand=True)
            else:
                frame.pack_forget()
        for name, item in self.sidebar_items.items():
            item.set_selected(name == key)
        self.current_page = key
        if self._screenshot:
            return
        if key == "big" and not self.big_rows:
            self.start_big_scan()
        if key == "dup" and not self.dup_groups:
            self.start_dup_scan()
        if key == "frag":
            self._update_frag_stats()
        if key == "migrate" and not self.mig_rows:
            self.start_migrate_scan()
        if key == "bigmove" and not self.bigmove_rows:
            self.start_bigmove_scan()
        if key == "wechat" and self.im_scan.get("wechat") is None:
            self._scan_im("wechat")
        if key == "qq" and self.im_scan.get("qq") is None:
            self._scan_im("qq")
        if key == "privacy" and not self.privacy_records and not self._privacy_running:
            self._start_privacy_scan()

    # ---------- 通用组件 ----------
    def _headline(self, parent, parts):
        frame = tk.Frame(parent, bg=WHITE)
        for text, color, bold, size in parts:
            font = ("Microsoft YaHei UI", size, "bold" if bold else "normal")
            tk.Label(frame, text=text, fg=color, bg=WHITE, font=font).pack(side="left")
        return frame

    def _drive_tabs(self, parent, values, variable, command):
        row = tk.Frame(parent, bg=WHITE)
        row.pack(fill="x", padx=18, pady=(6, 0))
        labels = {}

        def paint():
            for value, label in labels.items():
                if value == variable.get():
                    label.configure(fg=BLUE, bg=BLUE_BG,
                                    font=("Microsoft YaHei UI", 10, "bold"))
                else:
                    label.configure(fg=TEXT, bg=WHITE,
                                    font=("Microsoft YaHei UI", 10))

        def select(value):
            variable.set(value)
            paint()
            command()

        for value in values:
            display = value.rstrip("\\") if value.endswith(":\\") else value
            label = tk.Label(row, text=display, bg=WHITE, fg=TEXT, padx=12, pady=5,
                             cursor="hand2", font=("Microsoft YaHei UI", 10))
            label.pack(side="left", padx=(0, 6))
            label.bind("<Button-1>", lambda _e, v=value: select(v))
            labels[value] = label
        paint()
        return row

    def _header_row(self, parent, parts, button_text, command):
        row = tk.Frame(parent, bg=WHITE)
        row.pack(fill="x", padx=18, pady=(14, 6))
        left = tk.Frame(row, bg=WHITE)
        left.pack(side="left", fill="x", expand=True)
        self._headline(left, parts).pack(anchor="w")
        GreenButton(row, button_text, command).pack(side="right", pady=(4, 0))
        return row

    def _scrollable(self, parent, bg=WHITE):
        canvas = tk.Canvas(parent, bg=bg, highlightthickness=0)
        scroll = ttk.Scrollbar(parent, orient="vertical", command=canvas.yview)
        inner = tk.Frame(canvas, bg=bg)
        inner.bind("<Configure>", lambda _e: canvas.configure(scrollregion=canvas.bbox("all")))
        window = canvas.create_window((0, 0), window=inner, anchor="nw")
        canvas.bind("<Configure>", lambda e: canvas.itemconfigure(window, width=e.width))
        canvas.configure(yscrollcommand=scroll.set)
        canvas.pack(side="left", fill="both", expand=True)
        scroll.pack(side="right", fill="y")
        return inner

    def _card_grid(self, parent, cats, checked_set, on_toggle):
        grid = tk.Frame(parent, bg=WHITE)
        grid.pack(fill="x", padx=14, pady=(6, 10))
        columns = 8
        for index, cat in enumerate(cats):
            color = ICON_COLORS[index % len(ICON_COLORS)]
            card = tk.Frame(grid, bg=WHITE, highlightbackground=BORDER,
                            highlightthickness=1, width=92, height=108)
            card.grid(row=index // columns, column=index % columns, padx=4, pady=4)
            card.pack_propagate(False)
            top = tk.Frame(card, bg=WHITE)
            top.pack(fill="x", padx=6, pady=(6, 2))
            var = tk.BooleanVar(value=cat.key in checked_set)
            self._card_vars.append(var)
            cb = tk.Checkbutton(top, variable=var, bg=WHITE, activebackground=WHITE,
                                highlightthickness=0, bd=0,
                                command=lambda c=cat, v=var: on_toggle(c, v.get()))
            cb.pack(side="left")
            photo = self._app_icon(cat.name)
            if photo is not None:
                tk.Label(top, image=photo, bg=WHITE).pack(side="left", padx=(2, 0))
            else:
                IconTile(top, color, cat.name[:1], 30).pack(side="left", padx=(2, 0))
            name = tk.Label(card, text=cat.name, bg=WHITE, fg=TEXT,
                            font=("Microsoft YaHei UI", 8), wraplength=84, justify="center")
            name.pack(pady=(2, 0))
            size_text = self._size_text(cat.key)
            Pill(card, size_text, ORANGE).pack(pady=(3, 5))

    def _section_title(self, parent, text, note=""):
        row = tk.Frame(parent, bg=WHITE)
        row.pack(fill="x", padx=16, pady=(8, 2))
        tk.Label(row, text="● " + text, bg=WHITE, fg=TEXT,
                 font=("Microsoft YaHei UI", 10, "bold")).pack(side="left")
        if note:
            tk.Label(row, text=note, bg=WHITE, fg=MUTED,
                     font=("Microsoft YaHei UI", 9)).pack(side="left", padx=(10, 0))

    def _make_tree(self, parent, columns):
        frame = tk.Frame(parent, bg=WHITE)
        frame.pack(fill="both", expand=True, padx=14, pady=(6, 10))
        tree = ttk.Treeview(frame, columns=[c[0] for c in columns], show="headings",
                            selectmode="none")
        for key, title, width, anchor in columns:
            tree.heading(key, text=title)
            tree.column(key, width=width, anchor=anchor, stretch=(key == "name"))
        tree.tag_configure("alt", background=ROW_ALT)
        tree.pack(side="left", fill="both", expand=True)
        scroll = ttk.Scrollbar(frame, orient="vertical", command=tree.yview)
        scroll.pack(side="right", fill="y")
        tree.configure(yscrollcommand=scroll.set)
        return tree

    def _mark(self, checked):
        return "[x]" if checked else "[ ]"

    def _disk_of(self, path):
        drive, _rest = os.path.splitdrive(str(path))
        if drive:
            return drive
        text = str(path)
        if len(text) >= 2 and text[1] == ":":
            return text[:2]
        return ""

    def _fmt_time(self, stamp):
        try:
            stamp = float(stamp or 0)
        except (TypeError, ValueError):
            return "—"
        if stamp <= 0:
            return "—"
        return time.strftime("%Y/%m/%d %H:%M", time.localtime(stamp))

    def _kind_icon(self, path):
        kind = filetypes.kind_of(path)
        if kind == "installer":
            kind = "installer"
        cached = self._icon_cache.get(kind)
        if cached is None:
            cached = iconsutil.badge(kind, 16)
            self._icon_cache[kind] = cached
            self._photos.append(cached)
        return cached

    def _app_icon(self, label):
        exe = iconsutil.find_app_exe(label)
        if not exe:
            return None
        cached = self._icon_cache.get("app:" + exe)
        if cached is None:
            try:
                cached = iconsutil.file_icon(exe, 24)
            except Exception:
                cached = None
            self._icon_cache["app:" + exe] = cached
            if cached is not None:
                self._photos.append(cached)
        return cached

    def _write_clean_log(self, lines):
        try:
            history.write_log(LOG_DIR, lines)
        except OSError:
            pass

    def _confirm_paths(self, paths, title, note):
        paths = list(paths or [])
        if not paths:
            messagebox.showinfo("提示", "没有可处理的文件。")
            return False
        dialog = tk.Toplevel(self)
        dialog.title(title)
        dialog.transient(self)
        dialog.configure(bg=WHITE)
        dialog.geometry("560x420")
        tk.Label(dialog, text=note, bg=WHITE, fg=TEXT, justify="left", wraplength=520,
                 font=("Microsoft YaHei UI", 10)).pack(anchor="w", padx=16, pady=(14, 4))
        total = 0
        for path in paths:
            try:
                total += os.path.getsize(path)
            except OSError:
                pass
        tk.Label(dialog, text=f"共 {len(paths)} 个，约 {utils.fmt_size(total)}",
                 bg=WHITE, fg=RED, font=("Microsoft YaHei UI", 10, "bold")).pack(anchor="w", padx=16)
        box = tk.Text(dialog, height=14, bg="#f7f9fc", relief="flat",
                      font=("Microsoft YaHei UI", 9))
        box.pack(fill="both", expand=True, padx=16, pady=8)
        shown = paths[:80]
        box.insert("1.0", "\n".join(shown))
        if len(paths) > len(shown):
            box.insert("end", f"\n…… 另有 {len(paths) - len(shown)} 个")
        box.configure(state="disabled")
        result = {"ok": False}

        def accept(_event=None):
            result["ok"] = True
            dialog.destroy()

        def cancel(_event=None):
            dialog.destroy()

        buttons = tk.Frame(dialog, bg=WHITE)
        buttons.pack(fill="x", padx=16, pady=(0, 12))
        tk.Label(buttons, text="取消", bg=WHITE, fg=MUTED, cursor="hand2",
                 font=("Microsoft YaHei UI", 10)).pack(side="right", padx=(12, 0))
        buttons.winfo_children()[-1].bind("<Button-1>", cancel)
        go = tk.Label(buttons, text="确定", bg=GREEN, fg="white", padx=16, pady=4,
                      cursor="hand2", font=("Microsoft YaHei UI", 10, "bold"))
        go.pack(side="right")
        go.bind("<Button-1>", accept)
        dialog.protocol("WM_DELETE_WINDOW", cancel)
        dialog.grab_set()
        dialog.wait_window()
        return result["ok"]

    def _show_text_file(self, path):
        window = tk.Toplevel(self)
        window.title(os.path.basename(path))
        window.geometry("640x420")
        box = tk.Text(window, bg=WHITE, relief="flat")
        box.pack(fill="both", expand=True)
        try:
            with open(path, encoding="utf-8", errors="ignore") as handle:
                box.insert("1.0", handle.read())
        except OSError as exc:
            box.insert("1.0", str(exc))
        box.configure(state="disabled")

    def _show_clean_history(self):
        rows = history.load_history(LOG_DIR)
        window = tk.Toplevel(self)
        window.title("清理历史")
        window.geometry("560x360")
        window.configure(bg=WHITE)
        tree = ttk.Treeview(window, columns=("time", "freed", "items"), show="headings")
        for key, title, width in (("time", "时间", 180), ("freed", "释放", 120), ("items", "项目", 80)):
            tree.heading(key, text=title)
            tree.column(key, width=width, anchor="w")
        tree.pack(fill="both", expand=True, padx=8, pady=8)
        mapping = {}
        for index, row in enumerate(rows):
            iid = str(index)
            mapping[iid] = row["path"]
            tree.insert("", "end", iid=iid, values=(row["time"], row["freed"], row["items"]))
        if not rows:
            tk.Label(window, text="还没有清理记录。日志会写在程序目录的 logs 文件夹。",
                     bg=WHITE, fg=MUTED).pack(pady=6)

        def open_row(_event):
            iid = tree.focus()
            path = mapping.get(iid)
            if path:
                self._show_text_file(path)

        tree.bind("<Double-1>", open_row)

    # ---------- 页面：C盘瘦身 ----------
    def _page_clean(self, parent):
        self.clean_head = self._header_row(
            parent, [("正在统计", TEXT, True, 18)], "一键瘦身", self.start_clean)
        self.clean_sub = tk.Label(parent, text="", bg=WHITE, fg=MUTED,
                                  font=("Microsoft YaHei UI", 9))
        self.clean_sub.pack(anchor="w", padx=18)

        disk_row = tk.Frame(parent, bg=WHITE)
        disk_row.pack(fill="x", padx=18, pady=(4, 0))
        self.disk_bar = tk.Canvas(disk_row, width=430, height=12, bg=WHITE,
                                  highlightthickness=0)
        self.disk_bar.pack(side="left")
        self.disk_label = tk.Label(disk_row, text="", bg=WHITE, fg=MUTED,
                                   font=("Microsoft YaHei UI", 9))
        self.disk_label.pack(side="left", padx=(12, 0))
        self._draw_disk_bar()

        tabs = tk.Frame(parent, bg=WHITE)
        tabs.pack(fill="x", padx=18, pady=(12, 0))
        for index, (text, page) in enumerate((("系统盘清理", None), ("软件搬家", "migrate"),
                                              ("大文件搬家", "bigmove"))):
            color = BLUE if index == 0 else MUTED
            lbl = tk.Label(tabs, text=text, bg=WHITE, fg=color, padx=10, pady=4,
                           cursor="hand2",
                           font=("Microsoft YaHei UI", 10, "bold" if index == 0 else "normal"))
            lbl.pack(side="left", padx=(0, 10))
            if page:
                lbl.bind("<Button-1>", lambda _e, p=page: self._show_page(p))
        tk.Frame(parent, bg=BORDER, height=1).pack(fill="x", padx=18, pady=(6, 0))

        body = self._scrollable(parent)
        self.clean_body = body
        self._refresh_clean_cards()

    def _draw_disk_bar(self):
        try:
            total, used, free = scanner.scan_disk("C:\\")
        except Exception:
            return
        self.disk_bar.delete("all")
        width, height = 430, 12
        _round_rect(self.disk_bar, 0, 0, width, height, 6, fill="#eef1f6", outline="")
        fraction = used / total if total else 0
        if fraction > 0:
            _round_rect(self.disk_bar, 0, 0, max(12, int(width * fraction)), height, 6,
                        fill=GREEN, outline="")
        self.disk_label.configure(text=f"C: 共{utils.fmt_size(total)}，{utils.fmt_size(free)}可用")

    def _update_sidebar_sizes(self):
        clean_keys = {c.key for c in self._clean_categories()}
        clean_total = sum(self.results[k][0] for k in clean_keys
                          if k in self.results and self.results[k][0] > 0)
        privacy_keys = {"privacy_browser", "privacy_recent"}
        privacy_total = sum(self.results[k][0] for k in privacy_keys
                            if k in self.results and self.results[k][0] > 0)
        wechat_total = self._im_total("wechat")
        qq_total = self._im_total("qq")
        big_total = sum(row[1] for row in self.big_rows)
        dup_total = sum(g["size"] * max(len(g["files"]) - 1, 0) for g in self.dup_groups)
        privacy_count = sum(int(item.get("count") or 0) for item in self.privacy_records)
        frag_count = (self.frag_analysis or {}).get("count") or 0

        def text(value):
            return f"[{utils.fmt_size_compact(value)}]" if value else ""

        mapping = {
            "clean": text(clean_total),
            "frag": f"[{frag_count}]" if frag_count else "",
            "privacy": f"[{privacy_count}]" if privacy_count else text(privacy_total),
            "big": text(big_total), "dup": text(dup_total),
            "wechat": text(wechat_total), "qq": text(qq_total), "shred": "",
        }
        for key, value in mapping.items():
            item = self.sidebar_items.get(key)
            if item:
                item.set_size(value)

    def _im_total(self, key):
        files = self.im_files.get(key) or []
        if files:
            return sum(int(item.get("size") or 0) for item in files)
        data = self.im_scan.get(key) or {}
        if isinstance(data, dict):
            return sum(info.get("size", 0) for info in data.values() if isinstance(info, dict))
        return 0

    def _clean_categories(self):
        skip = {"privacy_browser", "privacy_recent", "app_wechat_cache",
                "app_qq_cache", "defrag"}
        return [c for c in self.categories if c.key not in skip]

    def _refresh_clean_cards(self):
        for child in self.clean_body.winfo_children():
            child.destroy()
        cats = self._clean_categories()
        recommended = [c for c in cats if not c.requires_confirm and c.tier != categories.DEEP]
        confirm = [c for c in cats if c.requires_confirm or c.tier == categories.DEEP]
        self._section_title(self.clean_body, "推荐清理项", "系统和软件的垃圾和无用文件，可放心清理")
        self._card_grid(self.clean_body, recommended, self.selected, self._toggle_clean)
        if confirm:
            self._section_title(self.clean_body, "需确认清理项", "数据缓存和不常用文件，可按需选择")
            self._card_grid(self.clean_body, confirm, self.selected, self._toggle_clean)

    def _toggle_clean(self, cat, value):
        if value:
            self.selected.add(cat.key)
        else:
            self.selected.discard(cat.key)
        self._update_clean_head()

    def _update_clean_head(self):
        total = sum(self.results[k][0] for k in self.selected
                    if k in self.results and self.results[k][0] > 0)
        found = sum(v[0] for v in self.results.values() if v and v[0] > 0)
        for child in self.clean_head.winfo_children():
            child.destroy()
        left = tk.Frame(self.clean_head, bg=WHITE)
        left.pack(side="left", fill="x", expand=True)
        self._headline(left, [
            ("发现 ", TEXT, False, 17), (utils.fmt_size(found), RED, True, 21),
            (" 可释放，已勾选 ", TEXT, False, 17), (utils.fmt_size(total), RED, True, 21),
        ]).pack(anchor="w")
        GreenButton(self.clean_head, "一键瘦身", self.start_clean).pack(side="right")
        self.clean_sub.configure(text=f"已选 {len(self.selected)} 项")

    def _size_text(self, key):
        item = self.results.get(key)
        if not item or item[0] is None or item[0] < 0:
            return "—"
        return utils.fmt_size(item[0])

    # ---------- 页面：碎片清理 ----------
    def _page_frag(self, parent):
        self._header_row(parent, [("清理系统碎片，加速电脑告别卡顿", TEXT, True, 17)],
                         "一键清理", self._run_defrag)
        self.frag_status = tk.Label(parent, text="状态：准备分析…", bg=WHITE, fg=MUTED,
                                    font=("Microsoft YaHei UI", 9))
        self.frag_status.pack(anchor="w", padx=18)

        self.frag_drive = tk.StringVar(value="C:\\")
        self._drive_tabs(parent, utils.fixed_drives(), self.frag_drive,
                         self._update_frag_stats)

        stats = tk.Frame(parent, bg=WHITE)
        stats.pack(fill="x", padx=30, pady=(16, 6))
        self.frag_values = {}
        for key, label in (("count", "碎片数"), ("space", "碎片文件"), ("rate", "碎片率")):
            box = tk.Frame(stats, bg=WHITE)
            box.pack(side="left", expand=True, anchor="w")
            tk.Label(box, text=label, bg=WHITE, fg=MUTED,
                     font=("Microsoft YaHei UI", 10)).pack(anchor="w")
            value = tk.Label(box, text="—", bg=WHITE, fg=ORANGE,
                             font=("Microsoft YaHei UI", 20, "bold"))
            value.pack(anchor="w", pady=(2, 0))
            self.frag_values[key] = value
        legend = tk.Frame(parent, bg=WHITE)
        legend.pack(fill="x", padx=30)
        tk.Label(legend, text="映射颜色", bg=WHITE, fg=MUTED,
                 font=("Microsoft YaHei UI", 8)).pack(side="right")
        tk.Label(legend, text="  碎片", bg=WHITE, fg="#2f9bf0",
                 font=("Microsoft YaHei UI", 8)).pack(side="right")
        tk.Label(legend, text="  正常", bg=WHITE, fg="#bcd9f5",
                 font=("Microsoft YaHei UI", 8)).pack(side="right")
        grid_canvas = tk.Canvas(parent, bg=WHITE, highlightthickness=0, height=280)
        grid_canvas.pack(fill="both", expand=True, padx=30, pady=(6, 10))
        self.frag_canvas = grid_canvas
        grid_canvas.bind("<Configure>", lambda _e: self._draw_frag_grid(grid_canvas))

    def _update_frag_stats(self):
        if self._frag_running:
            return
        self._frag_running = True
        drive = self.frag_drive.get()
        self.frag_status.configure(text=f"正在分析 {drive} 的碎片…")

        def work():
            exe = os.path.join(os.environ.get("SystemRoot", r"C:\Windows"),
                               "System32", "defrag.exe")
            report = ""
            try:
                proc = subprocess.run([exe, drive, "/A", "/V"], capture_output=True,
                                      creationflags=0x08000000, timeout=600)
                report = (proc.stdout or b"").decode("mbcs", "replace")
                if not report.strip():
                    report = (proc.stderr or b"").decode("mbcs", "replace")
            except Exception:
                report = ""
            self.queue.put(("frag_stats", fragviz.parse_defrag_report(report), drive))

        threading.Thread(target=work, daemon=True).start()

    def _draw_frag_grid(self, canvas):
        canvas.delete("all")
        width = canvas.winfo_width()
        if width < 40:
            return
        rows, cols = 11, 32
        cells = None
        if self.frag_analysis is not None:
            cells = fragviz.cells_from_analysis(self.frag_analysis.get("percent"), rows, cols)
        if not cells:
            canvas.create_text(width // 2, 80, text="分析完成后，这里按真实碎片率显示分布",
                               fill=MUTED, font=("Microsoft YaHei UI", 10))
            return
        cell = max(8, (width - 10) // cols)
        for index, flag in enumerate(cells):
            r, c = divmod(index, cols)
            color = "#4aa3ef" if flag == "hot" else "#d5e6f8"
            if flag == "hot" and (self.frag_analysis or {}).get("percent", 0) >= 30:
                color = "#2f9bf0"
            x = c * cell + 4
            y = r * cell + 4
            canvas.create_rectangle(x, y, x + cell - 3, y + cell - 3, fill=color, outline="")

    # ---------- 页面：隐私清理 ----------
    def _page_privacy(self, parent):
        self._header_row(parent, [("隐私记录", TEXT, True, 18)], "一键清理", self._privacy_clean)
        self.privacy_sub = tk.Label(parent, text="清理后防止泄露您的小秘密。已保存的密码不会删除。",
                                    bg=WHITE, fg=MUTED, font=("Microsoft YaHei UI", 9))
        self.privacy_sub.pack(anchor="w", padx=18)
        self.privacy_body = self._scrollable(parent)
        self._render_privacy()

    def _start_privacy_scan(self):
        if self._privacy_running:
            return
        self._privacy_running = True
        self.privacy_mode = "progress"
        self.privacy_records = []
        self.privacy_step = "准备扫描"
        self._render_privacy()

        def work():
            def progress(name, partial):
                self.queue.put(("privacy_progress", name, partial))

            try:
                records = privacy.scan(progress)
            except Exception:
                records = []
            self._privacy_running = False
            self.queue.put(("privacy_scan_done", records))

        threading.Thread(target=work, daemon=True).start()

    def _render_privacy(self):
        body = getattr(self, "privacy_body", None)
        if not body:
            return
        for child in body.winfo_children():
            child.destroy()
        if self.privacy_mode != "result":
            self._render_privacy_progress(body)
        else:
            self._render_privacy_result(body)

    def _render_privacy_progress(self, body):
        row = tk.Frame(body, bg=WHITE)
        row.pack(fill="both", expand=True, padx=20, pady=16)
        gauge = tk.Canvas(row, width=240, height=240, bg=WHITE, highlightthickness=0)
        gauge.pack(side="left", padx=(10, 20))
        seen = {record.get("section") for record in self.privacy_records}
        done = len([key for key, _title, _note in privacy.SECTIONS if key in seen])
        frac = done / max(len(privacy.SECTIONS), 1)
        gauge.create_oval(30, 30, 210, 210, outline="#e6eef8", width=16)
        if frac > 0:
            gauge.create_arc(30, 30, 210, 210, start=90, extent=-max(8, int(360 * frac)),
                             style="arc", outline=GREEN, width=16)
        gauge.create_text(120, 120, text=f"{int(frac * 100)}%", fill=TEXT,
                          font=("Microsoft YaHei UI", 18, "bold"))
        info = tk.Frame(row, bg=WHITE)
        info.pack(side="left", fill="both", expand=True, padx=10)
        for key, title, _note in privacy.SECTIONS:
            count = sum(int(item.get("count") or 0) for item in self.privacy_records
                        if item.get("section") == key)
            line = tk.Frame(info, bg=WHITE)
            line.pack(fill="x", pady=4)
            tk.Label(line, text=title, bg=WHITE, fg=TEXT,
                     font=("Microsoft YaHei UI", 10)).pack(side="left")
            shown = f"{count} 条" if key in seen else "等待扫描"
            tk.Label(line, text=shown, bg=WHITE, fg=ORANGE if key in seen else MUTED,
                     font=("Microsoft YaHei UI", 10, "bold")).pack(side="right")
        scanning = self.privacy_step or "准备扫描"
        tk.Label(info, text=f"正在扫描: {scanning}", bg=WHITE, fg=MUTED,
                 font=("Microsoft YaHei UI", 10)).pack(anchor="w", pady=(16, 0))

    def _render_privacy_result(self, body):
        total = sum(int(item.get("count") or 0) for item in self.privacy_records)
        chosen = sum(int(item.get("count") or 0) for item in self.privacy_records
                     if item.get("id") in self.privacy_selected)
        if getattr(self, "privacy_sub", None):
            self.privacy_sub.configure(text=f"共 {total} 条隐私记录，已选择 {chosen} 条。已保存的密码不会删除。")
        for key, title, note in privacy.SECTIONS:
            items = [item for item in self.privacy_records if item.get("section") == key]
            if not items:
                continue
            section_count = sum(int(item.get("count") or 0) for item in items)
            self._section_title(body, f"{title}（共{section_count}条）", note)
            grid = tk.Frame(body, bg=WHITE)
            grid.pack(fill="x", padx=14, pady=(2, 8))
            for index, item in enumerate(items):
                self._privacy_card(grid, item, index)

    def _privacy_card(self, grid, item, index):
        card = tk.Frame(grid, bg=WHITE, highlightbackground=BORDER, highlightthickness=1,
                        width=132, height=108)
        card.grid(row=index // 6, column=index % 6, padx=5, pady=4)
        card.pack_propagate(False)
        top = tk.Frame(card, bg=WHITE)
        top.pack(fill="x", padx=6, pady=(6, 2))
        var = tk.BooleanVar(value=item["id"] in self.privacy_selected)
        self._card_vars.append(var)
        tk.Checkbutton(top, variable=var, bg=WHITE, activebackground=WHITE,
                       highlightthickness=0, bd=0,
                       command=lambda i=item["id"], v=var: self._toggle_privacy(i, v.get())).pack(side="left")
        photo = self._app_icon(item.get("name", ""))
        if photo is not None:
            tk.Label(top, image=photo, bg=WHITE).pack(side="left")
        else:
            IconTile(top, ICON_COLORS[index % len(ICON_COLORS)], item.get("name", "?")[:1], 26).pack(side="left")
        tk.Label(card, text=item.get("name", ""), bg=WHITE, fg=TEXT,
                 font=("Microsoft YaHei UI", 8), wraplength=118, justify="center").pack()
        count = int(item.get("count") or 0)
        Pill(card, f"{count} 条", ORANGE if count else GREEN).pack(pady=(2, 4))

    def _toggle_privacy(self, key, value):
        if value:
            self.privacy_selected.add(key)
        else:
            self.privacy_selected.discard(key)
        if self.privacy_mode == "result":
            total = sum(int(item.get("count") or 0) for item in self.privacy_records)
            chosen = sum(int(item.get("count") or 0) for item in self.privacy_records
                         if item.get("id") in self.privacy_selected)
            if getattr(self, "privacy_sub", None):
                self.privacy_sub.configure(
                    text=f"共 {total} 条隐私记录，已选择 {chosen} 条。已保存的密码不会删除。")

    def _privacy_clean(self):
        if self._cleaning or self._privacy_running:
            return
        if not self.privacy_records:
            messagebox.showinfo("提示", "请等待隐私扫描完成。")
            return
        plan = privacy.clean_plan(self.privacy_records, self.privacy_selected)
        if not plan["files"] and not plan["sql_jobs"] and not plan["groups"] and not plan["extras"]:
            messagebox.showinfo("提示", "没有可清理的项目。")
            return
        note = "将清理勾选的痕迹。文件会移到回收站。下载记录和购物记录只删除浏览器库里的对应条目。已保存的密码不会动。"
        paths = plan["files"] or ["（本次没有需要移走的文件，只会清理记录）"]
        if not self._confirm_paths(paths, "确认清理隐私痕迹", note):
            return
        self._cleaning = True

        def work():
            try:
                result = privacy.apply_plan(plan)
            except Exception:
                result = {"files": 0, "sql": 0, "registry": 0, "failed": []}
            lines = ["开始清理", "清理：隐私痕迹",
                     f"清理结束，共释放 {result.get('files', 0)} 个文件"]
            self._write_clean_log(lines)
            self._cleaning = False
            self.queue.put(("privacy_cleaned", result))

        threading.Thread(target=work, daemon=True).start()

    # ---------- 页面：大文件专清 ----------
    def _page_big(self, parent):
        self.big_head = self._header_row(
            parent, [("正在扫描大文件", TEXT, True, 17)], "一键清理", self._delete_big)
        self.big_sub = tk.Label(parent, text="扫描磁盘中的大文件（>100 MB），删除前会先预览，默认移到回收站",
                                bg=WHITE, fg=MUTED, font=("Microsoft YaHei UI", 9))
        self.big_sub.pack(anchor="w", padx=18)
        self.big_scope = tk.StringVar(value="全部磁盘")
        self._drive_tabs(parent, ["全部磁盘"] + utils.fixed_drives(),
                         self.big_scope, self._rescan_big)
        tabs = tk.Frame(parent, bg=WHITE)
        tabs.pack(fill="x", padx=18, pady=(8, 0))
        self.big_tab_labels = {}
        for key in BIG_KINDS:
            label = tk.Label(tabs, text=key, bg=WHITE, fg=TEXT, padx=8, pady=4, cursor="hand2",
                             font=("Microsoft YaHei UI", 9))
            label.pack(side="left", padx=(0, 4))
            label.bind("<Button-1>", lambda _e, value=key: self._set_big_type(value))
            self.big_tab_labels[key] = label
        self.big_tree = self._make_tree(parent, [
            ("check", "", 36, "center"), ("name", "文件名称", 420, "w"),
            ("disk", "磁盘", 70, "center"), ("size", "文件大小", 90, "e"),
            ("mtime", "修改时间", 140, "center"),
        ])
        self.big_tree.configure(show="tree headings")
        self.big_tree.column("#0", width=28, stretch=False, anchor="center")
        self.big_tree.heading("size", command=lambda: self._sort_big("size"))
        self.big_tree.heading("mtime", command=lambda: self._sort_big("mtime"))
        self.big_tree.heading("name", command=lambda: self._sort_big("name"))
        self.big_tree.bind("<Button-1>", self._on_big_click)

    def _set_big_type(self, key):
        self.big_type = key
        self._render_big()

    def _sort_big(self, key):
        if self.big_sort_key == key:
            self.big_sort_desc = not self.big_sort_desc
        else:
            self.big_sort_key = key
            self.big_sort_desc = key != "name"
        self._render_big()

    def _big_kind(self, path):
        kind = filetypes.kind_of(path)
        return "other" if kind == "installer" else kind

    def _rescan_big(self):
        self.big_rows = []
        self.big_checked = set()
        self._render_big()
        self.start_big_scan()

    def start_big_scan(self):
        if self._big_running:
            return
        self._big_running = True
        self.big_sub.configure(text="正在扫描所选范围的大文件…")
        threading.Thread(target=self._big_worker, daemon=True).start()

    def _big_worker(self):
        scope = self.big_scope.get()
        drives = utils.fixed_drives() if scope == "全部磁盘" else [scope]
        rows = []
        for drive in drives:
            self.queue.put(("big_progress", drive))
            try:
                found = scanner.scan_large_files(drive, min_size=100 * 1024 * 1024, limit=500)
            except Exception:
                continue
            for path, size in found:
                try:
                    mtime = os.path.getmtime(path)
                except OSError:
                    mtime = 0
                rows.append((path, size, mtime))
        rows.sort(key=lambda item: item[1], reverse=True)
        system_names = {"pagefile.sys", "hiberfil.sys", "swapfile.sys",
                        "dumpstack.log.tmp", "memory.dmp"}
        rows = [row for row in rows
                if os.path.basename(row[0]).lower() not in system_names
                and not safepath.is_protected_path(row[0])]
        self._big_running = False
        self.queue.put(("big_rows", rows[:300]))

    def _render_big(self):
        if not getattr(self, "big_tree", None):
            return
        self.big_tree.delete(*self.big_tree.get_children())
        totals = {key: 0 for key in BIG_KINDS}
        for path, size, _mtime in self.big_rows:
            totals["all"] += size
            totals[self._big_kind(path)] = totals.get(self._big_kind(path), 0) + size
        names = {"all": "全部", "video": "视频", "music": "音乐", "document": "文档",
                 "image": "图片", "archive": "压缩包", "other": "其他"}
        for key, label in self.big_tab_labels.items():
            text = f"{names.get(key, key)}({utils.fmt_size_compact(totals.get(key, 0))})"
            if key == self.big_type:
                label.configure(text=text, bg=BLUE, fg="white")
            else:
                label.configure(text=text, bg=WHITE, fg=TEXT)
        visible = []
        for path, size, mtime in self.big_rows:
            if self.big_type != "all" and self._big_kind(path) != self.big_type:
                continue
            visible.append((path, size, mtime))

        def sort_value(item):
            path, size, mtime = item
            if self.big_sort_key == "mtime":
                return mtime
            if self.big_sort_key == "name":
                return os.path.basename(path).lower()
            return size

        visible.sort(key=sort_value, reverse=self.big_sort_desc)
        total = 0
        for index, (path, size, mtime) in enumerate(visible):
            total += size
            mark = self._mark(path in self.big_checked)
            disk = self._disk_of(path)
            self.big_tree.insert(
                "", "end", iid=str(index), text="", image=self._kind_icon(path),
                values=(mark, path, disk, utils.fmt_size(size), self._fmt_time(mtime)),
                tags=("alt",) if index % 2 else ())
        self._big_visible = [item[0] for item in visible]
        self._refresh_big_head(sum(row[1] for row in self.big_rows))

    def _refresh_big_head(self, total):
        for child in self.big_head.winfo_children():
            child.destroy()
        left = tk.Frame(self.big_head, bg=WHITE)
        left.pack(side="left", fill="x", expand=True)
        self._headline(left, [
            ("发现 ", TEXT, False, 17), (str(len(self.big_rows)), RED, True, 21),
            (" 个大文件，共 ", TEXT, False, 17), (utils.fmt_size(total), RED, True, 21),
        ]).pack(anchor="w")
        GreenButton(self.big_head, "一键清理", self._delete_big).pack(side="right")

    def _on_big_click(self, event):
        item = self.big_tree.identify_row(event.y)
        column = self.big_tree.identify_column(event.x)
        if not item or column not in ("#1", "#0"):
            return
        try:
            path = self._big_visible[int(item)]
        except (ValueError, IndexError):
            return
        if path in self.big_checked:
            self.big_checked.discard(path)
            mark = self._mark(False)
        else:
            self.big_checked.add(path)
            mark = self._mark(True)
        values = list(self.big_tree.item(item, "values"))
        values[0] = mark
        self.big_tree.item(item, values=values)

    def _delete_big(self):
        if self._cleaning or self._big_running:
            return
        paths = [path for path in self.big_checked if not safepath.is_protected_path(path)]
        if not paths:
            messagebox.showinfo("提示", "请先勾选要清理的大文件。系统文件不会被勾选删除。")
            return
        if not self._confirm_paths(paths, "确认清理大文件", recyclebin.describe() + "。确认后才会从原位置移走。"):
            return
        self._cleaning = True
        threading.Thread(target=self._recycle_worker, args=(paths, "大文件"), daemon=True).start()

    def _recycle_worker(self, paths, title):
        known = 0
        for path in paths:
            try:
                known += os.path.getsize(path)
            except OSError:
                pass
        failed = set(recyclebin.send_to_recycle(paths))
        done = [path for path in paths if path not in failed and not os.path.exists(path)]
        freed = 0
        for path in done:
            freed += 0
        # 送进回收站后原路径已经不在，大小用删除前统计里仍存在的失败项扣掉。
        failed_size = 0
        for path in failed:
            try:
                failed_size += os.path.getsize(path)
            except OSError:
                pass
        freed = max(known - failed_size, 0)
        self._write_clean_log([
            "开始清理", f"清理：{title}",
            f"清理结束，共释放 {utils.fmt_size(freed)}",
        ])
        self._cleaning = False
        self.queue.put(("recycled", title, len(done), len(failed), freed))

    # ---------- 页面：大文件搬家 ----------
    def _page_bigmove(self, parent):
        self._header_row(parent, [("大文件搬家", TEXT, True, 17)], "开始搬家", self._start_bigmove)
        tk.Label(parent, text="把 C 盘上的大文件复制到其他盘，校验通过后在原位置留下链接。失败会自动还原。",
                 bg=WHITE, fg=MUTED, font=("Microsoft YaHei UI", 9)).pack(anchor="w", padx=18)
        self.bigmove_sub = tk.Label(parent, text="尚未扫描", bg=WHITE, fg=MUTED,
                                    font=("Microsoft YaHei UI", 9))
        self.bigmove_sub.pack(anchor="w", padx=18)
        self.bigmove_tree = self._make_tree(parent, [
            ("check", "", 36, "center"), ("name", "文件名称", 460, "w"),
            ("size", "文件大小", 100, "e"), ("mtime", "修改时间", 150, "center"),
        ])
        self.bigmove_tree.bind("<Button-1>", self._on_bigmove_click)
        dest = tk.Frame(parent, bg=WHITE)
        dest.pack(fill="x", padx=18, pady=(0, 12))
        tk.Label(dest, text="搬到", bg=WHITE, fg=MUTED).pack(side="left")
        drives = utils.fixed_drives()
        default = next((item for item in drives if item.upper() != "C:\\"), drives[0] if drives else "D:\\")
        self.bigmove_dest = tk.StringVar(value=os.path.join(default, "MovedFiles"))
        tk.Entry(dest, textvariable=self.bigmove_dest, width=42).pack(side="left", padx=8)

    def start_bigmove_scan(self):
        if self._big_running:
            return
        self._big_running = True
        if getattr(self, "bigmove_sub", None):
            self.bigmove_sub.configure(text="正在查找 C 盘大文件…")

        def work():
            try:
                found = scanner.scan_large_files("C:\\", min_size=100 * 1024 * 1024, limit=200)
            except Exception:
                found = []
            rows = []
            for path, size in found:
                if safepath.is_system_path(path):
                    continue
                try:
                    mtime = os.path.getmtime(path)
                except OSError:
                    mtime = 0
                rows.append((path, size, mtime))
            self._big_running = False
            self.queue.put(("bigmove_rows", rows))

        threading.Thread(target=work, daemon=True).start()

    def _render_bigmove(self):
        tree = getattr(self, "bigmove_tree", None)
        if not tree:
            return
        tree.delete(*tree.get_children())
        self.bigmove_checked = set()
        total = 0
        for index, (path, size, mtime) in enumerate(self.bigmove_rows):
            total += size
            tree.insert("", "end", iid=str(index),
                        values=(self._mark(False), path, utils.fmt_size(size), self._fmt_time(mtime)),
                        tags=("alt",) if index % 2 else ())
        if getattr(self, "bigmove_sub", None):
            self.bigmove_sub.configure(text=f"C 盘 {len(self.bigmove_rows)} 个大文件，共 {utils.fmt_size(total)}")

    def _on_bigmove_click(self, event):
        item = self.bigmove_tree.identify_row(event.y)
        if not item or self.bigmove_tree.identify_column(event.x) != "#1":
            return
        path = self.bigmove_rows[int(item)][0]
        values = list(self.bigmove_tree.item(item, "values"))
        if path in self.bigmove_checked:
            self.bigmove_checked.discard(path)
            values[0] = self._mark(False)
        else:
            self.bigmove_checked.add(path)
            values[0] = self._mark(True)
        self.bigmove_tree.item(item, values=values)

    def _start_bigmove(self):
        if self._mig_running:
            return
        paths = list(self.bigmove_checked)
        if not paths:
            messagebox.showinfo("提示", "请先勾选要搬家的文件。")
            return
        dest = self.bigmove_dest.get().strip()
        if not dest:
            messagebox.showinfo("提示", "请填写目标文件夹。")
            return
        if not self._confirm_paths(paths, "确认搬家",
                                   "将先复制到目标盘并校验大小，再尝试在原位置建立链接。链接失败会把文件拷回原处。"):
            return
        self._mig_running = True

        def work():
            ok, errors = [], []
            for path in paths:
                try:
                    migrate.migrate_file(path, dest)
                    ok.append(path)
                except Exception as exc:
                    errors.append(f"{os.path.basename(path)}：{exc}")
            self._mig_running = False
            self.queue.put(("bigmove_done", ok, errors))

        threading.Thread(target=work, daemon=True).start()

    # ---------- 页面：重复文件专清 ----------
    def _page_dup(self, parent):
        self.dup_head = self._header_row(
            parent, [("正在查找重复文件", TEXT, True, 17)], "一键清理", self._delete_dup)
        self.dup_sub = tk.Label(parent, text="每组自动保留一份。删除前预览，默认移到回收站。",
                                bg=WHITE, fg=MUTED, font=("Microsoft YaHei UI", 9))
        self.dup_sub.pack(anchor="w", padx=18)
        self.dup_scope = tk.StringVar(value="全部磁盘")
        self._drive_tabs(parent, ["全部磁盘", "用户目录"] + utils.fixed_drives(),
                         self.dup_scope, self._rescan_dup)
        tabs = tk.Frame(parent, bg=WHITE)
        tabs.pack(fill="x", padx=18, pady=(8, 0))
        self.dup_tab_vars = {}
        labels = {"video": "视频", "music": "音乐", "document": "文档", "image": "图片",
                  "archive": "压缩包", "installer": "安装包", "other": "其他"}
        for key in DUP_KINDS:
            var = tk.BooleanVar(value=True)
            self._card_vars.append(var)
            self.dup_tab_vars[key] = var
            box = tk.Checkbutton(tabs, text=labels[key], variable=var, bg=WHITE,
                                 activebackground=WHITE, command=self._render_dup)
            box.pack(side="left", padx=(0, 6))
        clear = tk.Label(tabs, text="取消勾选", bg=BLUE_BG, fg=BLUE, padx=8, pady=2,
                         cursor="hand2", font=("Microsoft YaHei UI", 9))
        clear.pack(side="right")
        clear.bind("<Button-1>", lambda _e: self._dup_clear())
        self.dup_tree = self._make_tree(parent, [
            ("check", "", 36, "center"), ("name", "文件名称", 520, "w"),
            ("size", "文件大小", 100, "e"), ("mtime", "修改时间", 140, "center"),
        ])
        self.dup_tree.configure(show="tree headings")
        self.dup_tree.column("#0", width=28, stretch=False)
        self.dup_tree.tag_configure("group0", background="#eaf4fe")
        self.dup_tree.tag_configure("group1", background="#fff8ee")
        self.dup_tree.heading("size", command=lambda: self._sort_dup("size"))
        self.dup_tree.heading("mtime", command=lambda: self._sort_dup("mtime"))
        self.dup_tree.bind("<Button-1>", self._on_dup_click)

    def _dup_clear(self):
        self.dup_checked = set()
        self._render_dup()

    def _sort_dup(self, key):
        if self.dup_sort_key == key:
            self.dup_sort_desc = not self.dup_sort_desc
        else:
            self.dup_sort_key = key
            self.dup_sort_desc = True
        self._render_dup()

    def _rescan_dup(self):
        self.dup_groups = []
        self.dup_checked = set()
        self._render_dup()
        self.start_dup_scan()

    def start_dup_scan(self):
        if self._dup_running:
            return
        self._dup_running = True
        self._dup_cancel = False
        self.dup_sub.configure(text="正在查找重复文件…")
        threading.Thread(target=self._dup_worker, daemon=True).start()

    def _dup_worker(self):
        scope = self.dup_scope.get()
        if scope == "用户目录":
            roots = [os.environ.get("USERPROFILE", "C:\\")]
        elif scope == "全部磁盘":
            roots = utils.fixed_drives()
        else:
            roots = [scope]
        try:
            groups = dupes.find_duplicates(
                roots, min_size=1 << 20,
                progress=lambda n: self.queue.put(("dup_progress", n)),
                cancel=lambda: self._dup_cancel)
        except Exception:
            groups = []
        self._dup_running = False
        self.queue.put(("dup_rows", groups))

    def _group_kind(self, group):
        files = group.get("files") or []
        if not files:
            return "other"
        return filetypes.kind_of(files[0])

    def _render_dup(self):
        tree = getattr(self, "dup_tree", None)
        if not tree:
            return
        tree.delete(*tree.get_children())
        self.dup_paths = {}
        shown = []
        for group in self.dup_groups:
            kind = self._group_kind(group)
            var = self.dup_tab_vars.get(kind)
            if var is not None and not var.get():
                continue
            shown.append(group)
        if self.dup_sort_key == "mtime":
            shown.sort(key=lambda group: max((group.get("mtimes") or {}).values() or [0]),
                       reverse=self.dup_sort_desc)
        else:
            shown.sort(key=lambda group: group.get("size") or 0, reverse=self.dup_sort_desc)
        row = 0
        for group_index, group in enumerate(shown):
            tag = "group0" if group_index % 2 == 0 else "group1"
            for path in group.get("files") or []:
                iid = f"d{row}"
                self.dup_paths[iid] = path
                mark = self._mark(path in self.dup_checked)
                mtime = (group.get("mtimes") or {}).get(path, 0)
                tree.insert("", "end", iid=iid, text="", image=self._kind_icon(path),
                            values=(mark, path, utils.fmt_size(group.get("size") or 0), self._fmt_time(mtime)),
                            tags=(tag,))
                row += 1
        self._refresh_dup_head()

    def _refresh_dup_head(self):
        if not getattr(self, "dup_head", None):
            return
        doomed = dupes.paths_to_delete(self.dup_groups, self.dup_checked)
        sizes = {path: group["size"] for group in self.dup_groups for path in group.get("files") or []}
        selected_size = sum(sizes.get(path, 0) for path in doomed)
        reclaim = sum(group["size"] * max(len(group["files"]) - 1, 0) for group in self.dup_groups)
        for child in self.dup_head.winfo_children():
            child.destroy()
        left = tk.Frame(self.dup_head, bg=WHITE)
        left.pack(side="left", fill="x", expand=True)
        self._headline(left, [
            ("发现 ", TEXT, False, 17), (str(len(self.dup_groups)), RED, True, 21),
            (" 组重复文件，共 ", TEXT, False, 17), (utils.fmt_size(reclaim), RED, True, 21),
        ]).pack(anchor="w")
        GreenButton(self.dup_head, "一键清理", self._delete_dup).pack(side="right")
        if getattr(self, "dup_sub", None):
            self.dup_sub.configure(
                text=f"已勾选 {len(doomed)} 个重复文件，共 {utils.fmt_size(selected_size)}。每组至少保留一份。")

    def _on_dup_click(self, event):
        item = self.dup_tree.identify_row(event.y)
        column = self.dup_tree.identify_column(event.x)
        if not item or column not in ("#1", "#0"):
            return
        path = self.dup_paths.get(item)
        if not path:
            return
        if path in self.dup_checked:
            self.dup_checked.discard(path)
        else:
            self.dup_checked.add(path)
        for group in self.dup_groups:
            files = list(group.get("files") or [])
            if path in files and files and all(one in self.dup_checked for one in files):
                keeper = dupes.pick_keeper(group)
                self.dup_checked.discard(keeper)
                if getattr(self, "dup_sub", None):
                    self.dup_sub.configure(text="每组至少保留一个文件，已自动取消最后一份的勾选。")
        self._render_dup()

    def _delete_dup(self):
        if self._cleaning or self._dup_running:
            return
        paths = [path for path in dupes.paths_to_delete(self.dup_groups, self.dup_checked)
                 if not safepath.is_protected_path(path)]
        if not paths:
            messagebox.showinfo("提示", "请先勾选要清理的重复文件。每组都会留下至少一份。")
            return
        if not self._confirm_paths(paths, "确认清理重复文件",
                                   "每组都会保留至少一份。" + recyclebin.describe()):
            return
        self._cleaning = True
        threading.Thread(target=self._recycle_worker, args=(paths, "重复文件"), daemon=True).start()

    # ---------- 页面：微信 / QQ 专清 ----------
    def _page_wechat(self, parent):
        self._im_page(parent, "wechat", "微信文件")

    def _page_qq(self, parent):
        self._im_page(parent, "qq", "QQ 文件")

    def _im_page(self, parent, key, title):
        head = self._header_row(parent, [(title, TEXT, True, 18)], "一键清理",
                                lambda k=key: self._clean_im(k))
        self.im_heads[key] = head
        sub = tk.Label(parent, text="按月列出文件。只清理媒体、缓存和日志，不动聊天数据库。",
                       bg=WHITE, fg=MUTED, font=("Microsoft YaHei UI", 9))
        sub.pack(anchor="w", padx=18)
        self.im_subs[key] = sub
        body = self._scrollable(parent)
        self.im_bodies[key] = body
        self._refresh_im(key)

    def _refresh_im_head(self, key, files):
        head = self.im_heads.get(key)
        if not head:
            return
        total = sum(int(item.get("size") or 0) for item in files)
        title = "微信文件" if key == "wechat" else "QQ文件"
        for child in head.winfo_children():
            child.destroy()
        left = tk.Frame(head, bg=WHITE)
        left.pack(side="left", fill="x", expand=True)
        self._headline(left, [
            ("发现 ", TEXT, False, 17),
            (utils.fmt_size(total), RED, True, 21),
            (f" {title}", TEXT, True, 17),
        ]).pack(anchor="w")
        GreenButton(head, "一键清理", lambda k=key: self._clean_im(k)).pack(side="right")

    def _im_tabs(self, key):
        return imclean.WECHAT_TABS if key == "wechat" else imclean.QQ_TABS

    def _refresh_im(self, key):
        body = self.im_bodies.get(key)
        if not body:
            return
        for child in body.winfo_children():
            child.destroy()
        files = self.im_files.get(key) or []
        checked = self.im_checked_paths.get(key) or set()
        self._refresh_im_head(key, files)
        chosen = [item for item in files if item["path"] in checked]
        chosen_size = sum(item["size"] for item in chosen)
        sub = self.im_subs.get(key)
        if sub:
            if self._im_running.get(key) and not files:
                sub.configure(text="正在扫描…")
            else:
                name = "微信" if key == "wechat" else "QQ"
                sub.configure(text=f"已选择 {len(chosen)} 个{name}文件，合计 {utils.fmt_size(chosen_size)}")
        tabs = tk.Frame(body, bg=WHITE)
        tabs.pack(fill="x", padx=16, pady=(8, 4))
        current = self.im_tab.get(key) or self._im_tabs(key)[0][0]
        for kind, label in self._im_tabs(key):
            subset = [item for item in files if item["kind"] == kind]
            size = sum(item["size"] for item in subset)
            all_on = bool(subset) and all(item["path"] in checked for item in subset)
            piece = tk.Frame(tabs, bg=WHITE)
            piece.pack(side="left", padx=(0, 8))
            var = tk.BooleanVar(value=all_on)
            self._card_vars.append(var)
            tk.Checkbutton(piece, variable=var, bg=WHITE, activebackground=WHITE, bd=0,
                           command=lambda k=key, kd=kind, v=var: self._toggle_im_kind(k, kd, v.get())).pack(side="left")
            active = kind == current
            name = tk.Label(piece, text=f"{label}({utils.fmt_size_compact(size)})",
                            bg=BLUE if active else WHITE, fg="white" if active else TEXT,
                            padx=8, pady=3, cursor="hand2", font=("Microsoft YaHei UI", 9))
            name.pack(side="left")
            name.bind("<Button-1>", lambda _e, k=key, kd=kind: self._set_im_tab(k, kd))
        shown = [item for item in files if item["kind"] == current]
        if current == "file":
            self._render_received(body, key, shown)
        else:
            self._render_months(body, key, shown, current)

    def _refresh_im_soon(self, key):
        self.after(1, lambda k=key: self._refresh_im(k))

    def _set_im_tab(self, key, kind):
        self.im_tab[key] = kind
        self._refresh_im_soon(key)

    def _toggle_im_kind(self, key, kind, value):
        paths = {item["path"] for item in self.im_files.get(key) or [] if item["kind"] == kind}
        if value:
            self.im_checked_paths[key].update(paths)
        else:
            self.im_checked_paths[key].difference_update(paths)
        self._refresh_im_soon(key)

    def _toggle_im_paths(self, key, paths, value):
        if value:
            self.im_checked_paths[key].update(paths)
        else:
            self.im_checked_paths[key].difference_update(paths)
        self._refresh_im_soon(key)

    def _render_months(self, body, key, files, kind):
        groups = {}
        for item in files:
            groups.setdefault(item["month"], []).append(item)
        months = sorted(groups, reverse=True)
        if months and (key, "auto") not in self.im_open:
            self.im_open.add((key, months[0]))
            self.im_open.add((key, "auto"))
        for month in months:
            items = groups[month]
            label = items[0]["month_label"]
            self._group_header(body, key, label, items)
            if (key, month) not in self.im_open:
                continue
            limit = self.im_limits.get((key, month), 8)
            visible = items[:limit]
            if kind in ("image", "video", "sns"):
                self._thumb_grid(body, key, visible)
            else:
                self._file_lines(body, key, visible)
            if len(items) > limit:
                more = tk.Label(body, text="点击查看更多", bg=WHITE, fg=BLUE, cursor="hand2",
                                font=("Microsoft YaHei UI", 9))
                more.pack(pady=(2, 8))
                more.bind("<Button-1>", lambda _e, k=key, m=month: self._im_more(k, m))

    def _render_received(self, body, key, files):
        for sub, label in imclean.FILE_SUBS:
            items = [item for item in files if item.get("sub") == sub]
            self._group_header(body, key, label, items, group_id=("file", sub))
            if (key, "file", sub) not in self.im_open:
                continue
            limit = self.im_limits.get((key, "file", sub), 12)
            self._file_lines(body, key, items[:limit])
            if len(items) > limit:
                more = tk.Label(body, text="点击查看更多", bg=WHITE, fg=BLUE, cursor="hand2")
                more.pack()
                more.bind("<Button-1>", lambda _e, k=key, s=sub: self._im_more(k, ("file", s)))

    def _group_header(self, body, key, label, items, group_id=None):
        size = sum(item["size"] for item in items)
        paths = [item["path"] for item in items]
        checked = self.im_checked_paths[key]
        row = tk.Frame(body, bg=WHITE, highlightbackground=BORDER, highlightthickness=1)
        row.pack(fill="x", padx=16, pady=4)
        var = tk.BooleanVar(value=bool(paths) and all(path in checked for path in paths))
        self._card_vars.append(var)
        tk.Checkbutton(row, variable=var, bg=WHITE, activebackground=WHITE, bd=0,
                       command=lambda p=paths, v=var: self._toggle_im_paths(key, p, v.get())).pack(side="left", padx=6)
        title = tk.Label(row, text=label, bg=WHITE, fg=TEXT, cursor="hand2",
                         font=("Microsoft YaHei UI", 10))
        title.pack(side="left", padx=4, pady=8)
        tk.Label(row, text=utils.fmt_size(size), bg=WHITE, fg=MUTED).pack(side="right", padx=12)
        token = group_id if group_id is not None else (items[0]["month"] if items else label)
        title.bind("<Button-1>", lambda _e, k=key, token=token: self._toggle_open(k, token))

    def _toggle_open(self, key, token):
        marker = (key, token) if not isinstance(token, tuple) else (key,) + tuple(token)
        if isinstance(token, tuple):
            marker = (key,) + tuple(token)
        else:
            marker = (key, token)
        if marker in self.im_open:
            self.im_open.discard(marker)
        else:
            self.im_open.add(marker)
        self._refresh_im_soon(key)

    def _im_more(self, key, token):
        marker = (key,) + tuple(token) if isinstance(token, tuple) else (key, token)
        self.im_limits[marker] = self.im_limits.get(marker, 8) + 12
        self.im_open.add(marker)
        self._refresh_im_soon(key)

    def _file_lines(self, body, key, items):
        checked = self.im_checked_paths[key]
        for item in items:
            row = tk.Frame(body, bg=WHITE)
            row.pack(fill="x", padx=28, pady=1)
            var = tk.BooleanVar(value=item["path"] in checked)
            self._card_vars.append(var)
            tk.Checkbutton(row, variable=var, bg=WHITE, bd=0,
                           command=lambda p=item["path"], v=var: self._toggle_im_paths(key, [p], v.get())).pack(side="left")
            tk.Label(row, text=item["name"], bg=WHITE, fg=TEXT, anchor="w").pack(side="left", padx=4)
            tk.Label(row, text=self._fmt_time(item["mtime"]), bg=WHITE, fg=MUTED).pack(side="right", padx=8)
            tk.Label(row, text=utils.fmt_size(item["size"]), bg=WHITE, fg=MUTED).pack(side="right")

    def _thumb_grid(self, body, key, items):
        wrap = tk.Frame(body, bg=WHITE)
        wrap.pack(fill="x", padx=24, pady=4)
        checked = self.im_checked_paths[key]
        for index, item in enumerate(items):
            cell = tk.Frame(wrap, bg=WHITE, width=96, height=120)
            cell.grid(row=index // 6, column=index % 6, padx=4, pady=4)
            cell.pack_propagate(False)
            var = tk.BooleanVar(value=item["path"] in checked)
            self._card_vars.append(var)
            tk.Checkbutton(cell, variable=var, bg=WHITE, bd=0,
                           command=lambda p=item["path"], v=var: self._toggle_im_paths(key, [p], v.get())).pack(anchor="w")
            photo = self._load_thumb(item["path"])
            if photo is not None:
                tk.Label(cell, image=photo, bg="#f4f6f8").pack()
            else:
                box = tk.Canvas(cell, width=72, height=54, bg="#d9e7f5", highlightthickness=0)
                box.pack()
                box.create_text(36, 27, text=utils.fmt_size_compact(item["size"]), fill=TEXT,
                                font=("Microsoft YaHei UI", 8))
            tk.Label(cell, text=item["name"][:12], bg=WHITE, fg=MUTED,
                     font=("Microsoft YaHei UI", 7)).pack()

    def _load_thumb(self, path):
        ext = os.path.splitext(path)[1].lower()
        if ext not in (".png", ".gif", ".ppm", ".pgm"):
            return None
        try:
            image = tk.PhotoImage(file=path)
            factor = max(image.width() // 72, image.height() // 54, 1)
            if factor > 1:
                image = image.subsample(factor, factor)
            self._photos.append(image)
            return image
        except Exception:
            return None

    def _scan_im(self, key):
        if self._im_running.get(key):
            return
        self._im_running[key] = True
        sub = self.im_subs.get(key)
        if sub:
            sub.configure(text="正在扫描媒体文件…")

        def work():
            def progress(path):
                self.queue.put(("im_progress", key, path))

            try:
                records = imclean.list_media(self.im_roots[key](), progress=progress)
            except Exception:
                records = []
            self._im_running[key] = False
            self.queue.put(("im_scan", key, records))

        threading.Thread(target=work, daemon=True).start()

    def _clean_im(self, key):
        if self._cleaning:
            return
        files = self.im_files.get(key) or []
        chosen = [item for item in files if item["path"] in self.im_checked_paths.get(key, set())]
        if not chosen:
            messagebox.showinfo("提示", "请先勾选要清理的文件。")
            return
        paths = [item["path"] for item in chosen]
        if not self._confirm_paths(paths, "确认清理", "选中的媒体会移到回收站，聊天数据库不会动。"):
            return
        kinds = {item["kind"] for item in chosen}
        roots = self.im_roots[key]()
        self._cleaning = True

        def work():
            try:
                freed, deleted = imclean.clean(roots, kinds, paths=paths)
            except Exception:
                freed, deleted = 0, 0
            self._write_clean_log([
                "开始清理", f"清理：{'微信' if key == 'wechat' else 'QQ'}",
                f"清理结束，共释放 {utils.fmt_size(freed)}",
            ])
            self._cleaning = False
            self.queue.put(("im_done", key, freed, deleted))

        threading.Thread(target=work, daemon=True).start()

    # ---------- 页面：文件粉碎 ----------
    def _page_shred(self, parent):
        self._header_row(parent, [("彻底粉碎文件，远离残留", TEXT, True, 17)],
                         "一键粉碎", self._shred_selected)
        tk.Label(parent, text="先加入列表，确认后再覆盖写入并删除。系统目录不能粉碎，粉碎后无法恢复。",
                 bg=WHITE, fg=MUTED, font=("Microsoft YaHei UI", 9)).pack(anchor="w", padx=18)
        self.shred_tree = self._make_tree(parent, [
            ("check", "", 36, "center"), ("name", "文件名称", 180, "w"),
            ("path", "原路径", 360, "w"), ("mtime", "修改日期", 140, "center"),
            ("action", "操作", 70, "center"),
        ])
        self.shred_tree.bind("<Button-1>", self._on_shred_click)
        self.shred_empty = tk.Label(parent, text="点击添加或拖拽需要粉碎的文件/文件夹到这里",
                                    bg=WHITE, fg=MUTED, font=("Microsoft YaHei UI", 10))
        self.shred_empty.pack(pady=8)
        btns = tk.Frame(parent, bg=WHITE)
        btns.pack(fill="x", padx=18, pady=(0, 14))
        history_link = tk.Label(btns, text="查看粉碎历史", bg=WHITE, fg=BLUE, cursor="hand2",
                                font=("Microsoft YaHei UI", 9))
        history_link.pack(side="left")
        history_link.bind("<Button-1>", lambda _e: self._show_shred_history())
        for text, cmd in (("添加文件", self._shred_pick_file), ("添加文件夹", self._shred_pick_folder)):
            lbl = tk.Label(btns, text=text, bg=BLUE, fg="white", padx=16, pady=6,
                           cursor="hand2", font=("Microsoft YaHei UI", 9, "bold"))
            lbl.pack(side="right", padx=(8, 0))
            lbl.bind("<Button-1>", lambda _e, c=cmd: c())
        try:
            windrop.enable_file_drop(parent, self._shred_add_paths)
        except Exception:
            pass

    def _render_shred(self):
        tree = getattr(self, "shred_tree", None)
        if not tree:
            return
        tree.delete(*tree.get_children())
        for index, item in enumerate(self.shred_items):
            mark = self._mark(item.get("checked", True))
            tree.insert("", "end", iid=str(index), values=(
                mark, os.path.basename(item["path"]), item["path"],
                self._fmt_time(item.get("mtime")), "移除",
            ), tags=("alt",) if index % 2 else ())
        if getattr(self, "shred_empty", None):
            if self.shred_items:
                self.shred_empty.pack_forget()
            else:
                self.shred_empty.pack(pady=8)

    def _on_shred_click(self, event):
        item = self.shred_tree.identify_row(event.y)
        column = self.shred_tree.identify_column(event.x)
        if not item:
            return
        index = int(item)
        if column == "#5":
            del self.shred_items[index]
            self._render_shred()
            return
        if column in ("#1", "#0"):
            self.shred_items[index]["checked"] = not self.shred_items[index].get("checked", True)
            self._render_shred()

    def _shred_add_paths(self, paths):
        added = False
        for path in paths or []:
            if not path or not os.path.exists(path):
                continue
            if safepath.is_protected_path(path):
                messagebox.showerror("无法添加", f"为保护系统，不能粉碎：\n{path}")
                continue
            if any(item["path"] == path for item in self.shred_items):
                continue
            try:
                mtime = os.path.getmtime(path)
            except OSError:
                mtime = 0
            self.shred_items.append({"path": path, "mtime": mtime, "checked": True})
            added = True
        if added:
            self._render_shred()

    def _shred_pick_file(self):
        path = filedialog.askopenfilename(title="选择要粉碎的文件")
        if path:
            self._shred_add_paths([path])

    def _shred_pick_folder(self):
        path = filedialog.askdirectory(title="选择要粉碎的文件夹")
        if path:
            self._shred_add_paths([path])

    def _shred_selected(self):
        if self._cleaning:
            return
        paths = [item["path"] for item in self.shred_items if item.get("checked", True)]
        if not paths:
            messagebox.showinfo("提示", "请先添加要粉碎的文件或文件夹。")
            return
        blocked = [path for path in paths if safepath.is_protected_path(path)]
        if blocked:
            messagebox.showerror("无法粉碎", "列表中含有系统路径，已拒绝：\n" + "\n".join(blocked[:8]))
            return
        if not self._confirm_paths(paths, "确认粉碎", "将覆盖写入后永久删除，不能从回收站恢复。"):
            return
        self._cleaning = True
        threading.Thread(target=self._shred_worker, args=(paths,), daemon=True).start()

    def _shred_worker(self, paths):
        done = []
        for path in paths:
            try:
                shredder.shred_path(path)
                done.append(path)
            except Exception:
                continue
        if done:
            try:
                history.append_shred_history(LOG_DIR, done)
            except OSError:
                pass
        self._cleaning = False
        self.queue.put(("shred_done", done))

    def _show_shred_history(self):
        rows = history.load_shred_history(LOG_DIR)
        window = tk.Toplevel(self)
        window.title("粉碎历史")
        window.geometry("640x360")
        tree = ttk.Treeview(window, columns=("time", "path"), show="headings")
        tree.heading("time", text="时间")
        tree.heading("path", text="路径")
        tree.column("time", width=160)
        tree.column("path", width=440)
        tree.pack(fill="both", expand=True)
        for row in rows:
            tree.insert("", "end", values=(row["time"], row["path"]))
        if not rows:
            tk.Label(window, text="还没有粉碎记录。").pack()

    # ---------- 页面：软件搬家 ----------
    def _page_migrate(self, parent):
        self._header_row(parent, [("软件搬家", TEXT, True, 17)], "刷新列表",
                         self.start_migrate_scan)
        tk.Label(parent, text="从系统卸载信息里找出已安装软件。文档、桌面、下载不会出现在这里。",
                 bg=WHITE, fg=MUTED, font=("Microsoft YaHei UI", 9)).pack(anchor="w", padx=18)
        self.mig_tree = self._make_tree(parent, [
            ("name", "软件", 180, "w"), ("path", "目录", 430, "w"),
            ("size", "大小", 90, "e"), ("status", "状态", 80, "center")])
        dest = tk.Frame(parent, bg=WHITE)
        dest.pack(fill="x", padx=18, pady=(0, 12))
        tk.Label(dest, text="迁移到", bg=WHITE, fg=MUTED,
                 font=("Microsoft YaHei UI", 9)).pack(side="left")
        drives = utils.fixed_drives()
        default = next((item for item in drives if item.upper() != "C:\\"), drives[0] if drives else "D:\\")
        self.mig_dest_var = tk.StringVar(value=os.path.join(default, "MovedData"))
        tk.Entry(dest, textvariable=self.mig_dest_var, width=42).pack(side="left", padx=(6, 14))
        for text, cmd, color in (("迁移选中", self._mig_start, GREEN),
                                 ("撤销迁移", self._mig_undo, BLUE)):
            lbl = tk.Label(dest, text=text, bg=color, fg="white", padx=14, pady=5,
                           cursor="hand2", font=("Microsoft YaHei UI", 9, "bold"))
            lbl.pack(side="left", padx=(0, 8))
            lbl.bind("<Button-1>", lambda _e, c=cmd: c())


    # ---------- 动作 ----------
    def start_scan(self):
        if self._scanning:
            return
        self.scan_complete = False
        self._scanning = True
        threading.Thread(target=self._scan_worker, daemon=True).start()

    def _scan_worker(self):
        for cat in self.categories:
            try:
                size, count = scanner.scan_category(cat)
            except Exception:
                size, count = -1, 0
            self.queue.put(("scanned", cat.key, size, count))
        self._scanning = False
        self.queue.put(("scan_done",))

    def start_clean(self):
        if not self.scan_complete:
            messagebox.showinfo("提示", "请等待扫描完成后再清理。")
            return
        chosen = [c for c in self.categories if c.key in self.selected]
        if not chosen:
            messagebox.showinfo("提示", "请先勾选要清理的项目。")
            return
        lines = [f"即将清理 {len(chosen)} 项：", ""]
        for cat in chosen:
            lines.append(f"· {cat.name}  {self._size_text(cat.key)}")
        deep = [c for c in chosen if c.requires_confirm]
        if deep:
            lines.append("")
            lines.append("以下操作不可恢复：")
            for cat in deep:
                lines.append(f"· {cat.name}：{cat.warning or cat.note or cat.describe}")
        lines.extend(["", "确定继续？"])
        if self._cleaning:
            return
        if not messagebox.askyesno("确认清理", "\n".join(lines), icon="warning"):
            return
        self._cleaning = True
        threading.Thread(target=self._clean_worker, args=(chosen,), daemon=True).start()

    def _clean_keys(self, keys):
        chosen = [c for c in self.categories if c.key in keys]
        if not chosen:
            messagebox.showinfo("提示", "没有可清理的项目。")
            return
        if self._cleaning:
            return
        if not messagebox.askyesno("确认清理", "确定清理这些项目？", icon="warning"):
            return
        self._cleaning = True
        threading.Thread(target=self._clean_worker, args=(chosen,), daemon=True).start()

    def _clean_worker(self, chosen):
        lines = ["开始清理"]
        freed = 0
        for cat in chosen:
            try:
                result = cleaner.clean_category(cat, log=lines.append)
                freed += result.freed
                lines.append(f"清理：{cat.name}")
            except Exception as exc:
                lines.append(f"清理失败：{cat.name} {exc}")
        lines.append(f"清理结束，共释放 {utils.fmt_size(freed)}")
        self._write_clean_log(lines)
        self._cleaning = False
        self.queue.put(("clean_done", freed))

    def _run_defrag(self):
        if self._cleaning:
            return
        drive = self.frag_drive.get()
        if not messagebox.askyesno("确认", f"将对 {drive} 执行碎片整理/优化，可能需要较长时间。",
                                   icon="warning"):
            return
        self._cleaning = True
        threading.Thread(target=self._defrag_worker, args=(drive,), daemon=True).start()

    def _defrag_worker(self, drive):
        exe = os.path.join(os.environ.get("SystemRoot", r"C:\Windows"), "System32", "defrag.exe")
        try:
            proc = subprocess.run([exe, drive, "/O"], capture_output=True,
                                  creationflags=0x08000000, timeout=3600)
            ok = proc.returncode == 0
        except Exception:
            ok = False
        self._cleaning = False
        self.queue.put(("defrag_done", ok))

    def start_migrate_scan(self):
        if self._mig_running:
            return
        self._mig_running = True
        threading.Thread(target=self._mig_scan_worker, daemon=True).start()

    def _mig_scan_worker(self):
        rows = []
        try:
            found = migrate.discover_installed()
        except Exception:
            found = []
        for item in found:
            path = item["path"]
            try:
                status = "已是联接" if migrate.is_junction(path) else "可迁移"
            except Exception:
                status = "可迁移"
            rows.append((item.get("name") or os.path.basename(path), path, item.get("size") or 0, status))
        self._mig_running = False
        self.queue.put(("mig_rows", rows))

    def _render_mig(self, rows):
        self.mig_rows = rows
        self.mig_tree.delete(*self.mig_tree.get_children())
        for index, (name, path, size, status) in enumerate(rows):
            self.mig_tree.insert("", "end", iid=str(index),
                                 values=(name, path, utils.fmt_size(size) if size else "—", status),
                                 tags=("alt",) if index % 2 else ())

    def _mig_selected(self):
        item = self.mig_tree.focus()
        if not item:
            return None
        return self.mig_rows[int(item)]

    def _mig_start(self):
        if self._mig_running:
            return
        row = self._mig_selected()
        if not row:
            messagebox.showinfo("提示", "请先在列表中选择一个目录。")
            return
        _name, src, _size, status = row
        if status != "可迁移":
            messagebox.showinfo("提示", "该目录当前不可迁移。")
            return
        dest = self.mig_dest_var.get().strip()
        if not dest:
            messagebox.showinfo("提示", "请填写目标文件夹。")
            return
        if not messagebox.askyesno("确认迁移",
                                   f"将迁移：\n{src}\n→ {dest}\n\n请先关闭相关程序。确定继续？",
                                   icon="warning"):
            return
        self._mig_running = True
        threading.Thread(target=self._mig_worker, args=(src, dest), daemon=True).start()

    def _mig_worker(self, src, dest):
        try:
            migrate.migrate(src, dest)
            self._mig_running = False
            self.queue.put(("mig_done", True, src))
        except Exception as exc:
            self._mig_running = False
            self.queue.put(("mig_done", False, str(exc)))

    def _mig_undo(self):
        if self._mig_running:
            return
        row = self._mig_selected()
        if not row:
            messagebox.showinfo("提示", "请先在列表中选择一个目录。")
            return
        _name, src, _size, status = row
        if status != "已是联接":
            messagebox.showinfo("提示", "只能撤销处于“已是联接”状态的目录。")
            return
        if not messagebox.askyesno("确认撤销", f"将把数据搬回原位置：\n{src}\n确定继续？",
                                   icon="warning"):
            return
        self._mig_running = True
        threading.Thread(target=self._mig_undo_worker, args=(src,), daemon=True).start()

    def _mig_undo_worker(self, src):
        try:
            migrate.undo(src)
            self._mig_running = False
            self.queue.put(("mig_done", True, src))
        except Exception as exc:
            self._mig_running = False
            self.queue.put(("mig_done", False, str(exc)))

    def _check_rules_update(self):
        def work():
            added, total, err = ruleupdate.update_rules()
            self.queue.put(("rules_done", added, total, err))
        threading.Thread(target=work, daemon=True).start()

    # ---------- 队列 ----------
    def _poll(self):
        try:
            while True:
                try:
                    self._handle(self.queue.get_nowait())
                except queue.Empty:
                    break
                except Exception:
                    continue
        except queue.Empty:
            pass
        self.after(100, self._poll)

    def _handle(self, message):
        kind = message[0]
        if kind == "scanned":
            _, key, size, count = message
            self.results[key] = (size, count)
            self._update_clean_head()
            self._update_sidebar_sizes()
        elif kind == "scan_done":
            self.worker = None
            self.scan_complete = True
            self._refresh_clean_cards()
            self._update_clean_head()
            self._update_sidebar_sizes()
        elif kind == "clean_done":
            self.worker = None
            self._draw_disk_bar()
            messagebox.showinfo("清理完成", f"本次释放 {utils.fmt_size(message[1])}")
            self.start_scan()
        elif kind == "big_progress":
            self.big_sub.configure(text=f"正在扫描 {message[1]} 的大文件…")
        elif kind == "big_rows":
            self.worker = None
            self.big_rows = message[1]
            self._render_big()
            self._update_sidebar_sizes()
        elif kind == "dup_progress":
            self.dup_sub.configure(text=f"正在查找重复文件… 已发现 {message[1]} 组")
        elif kind == "dup_rows":
            self.worker = None
            self.dup_groups = message[1]
            self.dup_checked = dupes.auto_select(self.dup_groups)
            self._render_dup()
            self._update_sidebar_sizes()
        elif kind == "defrag_done":
            self.worker = None
            self._update_frag_stats()
            messagebox.showinfo("完成", "磁盘优化已结束。")
        elif kind == "shred_done":
            self.worker = None
            done = message[1]
            paths = done if isinstance(done, (list, tuple)) else []
            count = len(paths) if isinstance(done, (list, tuple)) else int(done or 0)
            finished = set(paths)
            self.shred_items = [item for item in self.shred_items if item["path"] not in finished]
            self._render_shred()
            messagebox.showinfo("完成", f"已粉碎 {count} 个文件。")
        elif kind == "recycled":
            _title, done, failed, freed = message[1], message[2], message[3], message[4]
            self.big_checked.clear()
            self.dup_checked.clear()
            messagebox.showinfo("完成", f"{_title}：已移入回收站 {done} 个，失败 {failed} 个，约 {utils.fmt_size(freed)}。")
            if self.current_page == "big":
                self.start_big_scan()
            elif self.current_page == "dup":
                self.start_dup_scan()
        elif kind == "bigmove_rows":
            self.bigmove_rows = message[1]
            self._render_bigmove()
        elif kind == "bigmove_done":
            ok, errors = message[1], message[2]
            text = f"已搬家 {len(ok)} 个文件。"
            if errors:
                text += "\n" + "\n".join(errors[:8])
            messagebox.showinfo("搬家结果", text)
            self.start_bigmove_scan()
        elif kind == "privacy_progress":
            self.privacy_step = message[1]
            self.privacy_records = message[2]
            self.privacy_mode = "progress"
            self._render_privacy()
        elif kind == "privacy_scan_done":
            self.privacy_records = message[1]
            self.privacy_mode = "result"
            self.privacy_selected = {
                item["id"] for item in self.privacy_records if int(item.get("count") or 0) > 0
            }
            self._render_privacy()
            self._update_sidebar_sizes()
        elif kind == "privacy_cleaned":
            result = message[1]
            messagebox.showinfo(
                "清理完成",
                f"移走 {result.get('files', 0)} 个文件，处理 {result.get('sql', 0)} 个数据库，"
                f"清除 {result.get('registry', 0)} 条记录。")
            self.privacy_records = []
            self.privacy_selected = set()
            self._start_privacy_scan()
        elif kind == "mig_rows":
            self.worker = None
            self._render_mig(message[1])
        elif kind == "mig_done":
            self.worker = None
            if message[1]:
                messagebox.showinfo("完成", f"已处理：{message[2]}")
                self.start_migrate_scan()
            else:
                messagebox.showerror("失败", message[2])
        elif kind == "privacy_done":
            self.worker = None
            self.reg_counts = {k: registry.group_count(k) for k, _ in registry.GROUPS}
            self._refresh_privacy_cards()
            messagebox.showinfo("清理完成",
                                f"释放 {utils.fmt_size(message[1])}，清除 {message[2]} 条注册表记录")
        elif kind == "im_progress":
            sub = self.im_subs.get(message[1])
            if sub:
                sub.configure(text="正在扫描: " + str(message[2]))
        elif kind == "im_scan":
            _, key, records = message
            self.im_files[key] = records or []
            self.im_scan[key] = imclean.aggregate(records or [])
            self.im_checked_paths[key] = {
                item["path"] for item in self.im_files[key] if item.get("kind") in ("cache", "log")
            }
            self._refresh_im(key)
            self._update_sidebar_sizes()
        elif kind == "frag_stats":
            _, report, drive = message
            self.frag_analysis = report or {}
            self._frag_running = False
            count = self.frag_analysis.get("count")
            space = self.frag_analysis.get("space_gb")
            percent = self.frag_analysis.get("percent")
            self.frag_values["count"].configure(text=str(count) if count is not None else "—")
            self.frag_values["space"].configure(text=f"{space} GB" if space is not None else "—")
            self.frag_values["rate"].configure(text=f"{percent:g}%" if percent is not None else "—")
            if percent is None and count is None:
                self.frag_status.configure(text="状态：没能读到碎片分析，分布图不会用假数据填充")
            else:
                self.frag_status.configure(text=f"状态：{str(drive).rstrip(chr(92))} 分析完成")
            if self.frag_canvas is not None:
                self._draw_frag_grid(self.frag_canvas)
            self._update_sidebar_sizes()
        elif kind == "im_done":
            _, key, freed, deleted = message
            self.worker = None
            messagebox.showinfo("完成", f"清理 {deleted} 个文件，释放 {utils.fmt_size(freed)}")
            self.im_files[key] = []
            self.im_checked_paths[key] = set()
            self.im_scan[key] = None
            self._scan_im(key)
        elif kind == "rules_done":
            _, added, total, err = message
            if err and not added:
                messagebox.showinfo("规则更新", err)
            else:
                extra = f"\n{err}" if err else ""
                messagebox.showinfo("规则更新", f"新增 {added} 条，共 {total} 条。{extra}\n重启工具后生效。")


    def _load_screenshot_demo(self):
        """CLEANER_SCREENSHOT=1 时填入示例数据，方便在没有 Windows 磁盘时看页面结构。"""
        now = time.time()
        for index, cat in enumerate(self.categories[:12]):
            self.results[cat.key] = ((index + 1) * 80 * 1024 * 1024, index + 2)
            if cat.tier != categories.DEEP:
                self.selected.add(cat.key)
        self._refresh_clean_cards()
        self._update_clean_head()
        self.big_rows = [
            (r"C:\Videos\holiday.mp4", 800 * 1024 * 1024, now),
            (r"D:\Docs\spec.pdf", 120 * 1024 * 1024, now - 86400),
            (r"E:\Music\album.flac", 150 * 1024 * 1024, now - 3600),
            (r"C:\Pictures\shot.png", 140 * 1024 * 1024, now - 7200),
            (r"D:\Backup\data.zip", 900 * 1024 * 1024, now - 10000),
            (r"C:\Work\notes.dwg", 220 * 1024 * 1024, now - 50000),
        ]
        self.big_checked = set()
        self._render_big()
        self.bigmove_rows = [row for row in self.big_rows if str(row[0]).startswith("C:")]
        self._render_bigmove()
        self.dup_groups = [
            {"hash": "a", "size": 3500 * 1024 * 1024,
             "files": [r"E:\移入\资料.rar", r"E:\备份\资料.rar"],
             "mtimes": {r"E:\移入\资料.rar": now, r"E:\备份\资料.rar": now - 86400}},
            {"hash": "b", "size": 756 * 1024 * 1024,
             "files": [r"D:\soft\setup.exe", r"E:\soft\setup.exe"],
             "mtimes": {r"D:\soft\setup.exe": now - 1000, r"E:\soft\setup.exe": now - 2000}},
        ]
        self.dup_checked = dupes.auto_select(self.dup_groups)
        self._render_dup()
        sample = []
        for month, label, kind, size in (
            ("2026-11", "2026年11月聊天视频", "video", 12 * 1024 * 1024),
            ("2026-11", "2026年11月聊天视频", "video", 8 * 1024 * 1024),
            ("2026-10", "2026年10月聊天视频", "video", 19 * 1024 * 1024),
            ("2026-11", "2026年11月聊天图片", "image", 2 * 1024 * 1024),
            ("2026-11", "2026年11月", "file", 10 * 1024 * 1024),
        ):
            sub = "doc" if kind == "file" else ""
            sample.append({
                "path": rf"D:\xwechat_files\wxid\msg\{kind}\{label}-{size}",
                "size": size, "mtime": now, "kind": kind, "sub": sub,
                "month": month, "month_label": label if kind != "file" else "2026年11月接收文件",
                "name": f"{kind}-{size // 1024}k",
            })
        sample.append({
            "path": r"D:\xwechat_files\wxid\cache\tmp.dat", "size": 80 * 1024 * 1024,
            "mtime": now, "kind": "cache", "sub": "", "month": "2026-11",
            "month_label": "2026年11月缓存", "name": "tmp.dat",
        })
        self.im_files["wechat"] = sample
        self.im_scan["wechat"] = imclean.aggregate(sample)
        self.im_checked_paths["wechat"] = {item["path"] for item in sample if item["kind"] == "cache"}
        self.im_tab["wechat"] = "video"
        self._refresh_im("wechat")
        qq = [dict(item, path=item["path"].replace("xwechat", "qq"), kind="image",
                   month_label="2026年11月聊天图片") for item in sample if item["kind"] == "image"]
        self.im_files["qq"] = qq
        self.im_scan["qq"] = imclean.aggregate(qq)
        self.im_checked_paths["qq"] = set()
        self.im_tab["qq"] = "image"
        self._refresh_im("qq")
        self.frag_analysis = {"percent": 33.44, "space_gb": 18.99, "count": 135895, "free_gb": 56.6}
        self.frag_values["count"].configure(text="135895")
        self.frag_values["space"].configure(text="18.99 GB")
        self.frag_values["rate"].configure(text="33.44%")
        self.frag_status.configure(text="状态：Win10(C:) 分析完成")
        if self.frag_canvas is not None:
            self.frag_canvas.update_idletasks()
            self._draw_frag_grid(self.frag_canvas)
        self.privacy_records = [
            {"id": "cookies", "section": "high", "name": "Cookie / 登录状态", "count": 16, "files": [], "action": "files"},
            {"id": "recent_files", "section": "high", "name": "最近文档", "count": 1, "files": [], "action": "files"},
            {"id": "hist_chrome", "section": "history", "name": "Chrome", "count": 2062, "files": [], "action": "files"},
            {"id": "hist_edge", "section": "history", "name": "Microsoft Edge", "count": 89, "files": [], "action": "files"},
            {"id": "hist_qqbrowser", "section": "history", "name": "QQ浏览器", "count": 24, "files": [], "action": "files"},
            {"id": "download", "section": "download", "name": "下载历史记录", "count": 147, "files": [], "action": "sql"},
            {"id": "shop", "section": "shop", "name": "网络购物记录", "count": 12, "files": [], "action": "sql"},
            {"id": "reg_file", "section": "computer", "name": "文件访问记录", "count": 711, "files": [], "action": "registry"},
            {"id": "reg_system", "section": "computer", "name": "系统使用痕迹", "count": 132, "files": [], "action": "registry"},
            {"id": "reg_usb", "section": "computer", "name": "USB 使用痕迹", "count": 6, "files": [], "action": "registry"},
            {"id": "reg_search", "section": "computer", "name": "电脑搜索记录", "count": 0, "files": [], "action": "registry"},
            {"id": "software", "section": "software", "name": "软件使用痕迹", "count": 40, "files": [], "action": "userassist"},
            {"id": "media", "section": "media", "name": "影音播放痕迹", "count": 8, "files": [], "action": "media"},
        ]
        self.privacy_mode = "result"
        self.privacy_selected = {item["id"] for item in self.privacy_records if item["count"]}
        self._render_privacy()
        self.mig_rows = [("演示软件", r"C:\Program Files\Demo", 12 * 1024 * 1024, "可迁移")]
        self._render_mig(self.mig_rows)
        self._update_sidebar_sizes()
        self.scan_complete = True

def main():
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(1)
    except Exception:
        pass
    if not utils.is_admin():
        if utils.relaunch_as_admin():
            return
    app = App()
    app.mainloop()


if __name__ == "__main__":
    main()
