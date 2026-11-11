"""C 盘清理工具 —— 图形界面（参考金山毒霸磁盘管理风格）。"""
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
import history
import imclean
import migrate
import registry
import ruleupdate
import scanner
import shredder
import utils

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
SIDEBAR_W = 168

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
        super().__init__(master, bg=WHITE, cursor="hand2")
        self.key = key
        self._command = command
        self._selected = False
        self._inner = tk.Frame(self, bg=WHITE)
        self._inner.pack(fill="x", padx=8, pady=2)
        self.icon = IconTile(self._inner, color, symbol, 26)
        self.icon.pack(side="left", padx=(6, 8))
        self.label = tk.Label(self._inner, text=label, bg=WHITE, fg=TEXT,
                              font=("Microsoft YaHei UI", 10))
        self.label.pack(side="left")
        self.size_label = tk.Label(self._inner, text="", bg=WHITE, fg=MUTED,
                                   font=("Microsoft YaHei UI", 8))
        self.size_label.pack(side="left", padx=(4, 0))
        for widget in (self, self._inner, self.icon, self.label):
            widget.bind("<Button-1>", lambda _e: self._command(key))

    def set_size(self, text):
        self.size_label.configure(text=text)

    def set_selected(self, selected):
        self._selected = selected
        bg = BLUE_BG if selected else WHITE
        fg = BLUE if selected else TEXT
        self.configure(bg=bg)
        self._inner.configure(bg=bg)
        self.icon.configure(bg=bg)
        self.label.configure(bg=bg, fg=fg)
        self.size_label.configure(bg=bg)


class App(tk.Tk):
    def __init__(self):
        super().__init__()
        self.title("毒霸磁盘管理")
        self.geometry("1000x690")
        self.minsize(940, 640)
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

        self._build_style()
        self._build_ui()
        self._show_page("clean")
        self.after(100, self._poll)
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
        self.sidebar = tk.Frame(self, bg=WHITE, width=SIDEBAR_W)
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
        top = tk.Frame(self.sidebar, bg=WHITE)
        top.pack(fill="x", pady=(14, 10), padx=10)
        logo = tk.Canvas(top, width=30, height=30, bg=WHITE, highlightthickness=0)
        logo.pack(side="left")
        logo.create_oval(2, 2, 28, 28, fill="#2f80ed", outline="")
        logo.create_text(15, 15, text="C", fill="white", font=("Arial", 12, "bold"))
        tk.Label(top, text="毒霸磁盘管理", bg=WHITE, fg=TEXT,
                 font=("Microsoft YaHei UI", 12, "bold")).pack(side="left", padx=(8, 0))

        for key, label, symbol, color in SIDEBAR:
            item = SidebarItem(self.sidebar, key, label, symbol, color, self._show_page)
            item.pack(fill="x", pady=1)
            self.sidebar_items[key] = item

        bottom = tk.Frame(self.sidebar, bg=WHITE)
        bottom.pack(side="bottom", fill="x", pady=10, padx=14)
        rules = tk.Label(bottom, text="规则更新", bg=WHITE, fg=BLUE, cursor="hand2",
                         font=("Microsoft YaHei UI", 9))
        rules.pack()
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
        if key == "big" and not self.big_rows:
            self.start_big_scan()
        if key == "dup" and not self.dup_groups:
            self.start_dup_scan()
        if key == "frag":
            self._update_frag_stats()
        if key == "migrate" and not self.mig_rows:
            self.start_migrate_scan()
        if key == "wechat" and self.im_scan.get("wechat") is None:
            self._scan_im("wechat")
        if key == "qq" and self.im_scan.get("qq") is None:
            self._scan_im("qq")

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
                                              ("大文件搬家", "big"))):
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
        big_total = sum(size for _, size in self.big_rows)
        dup_total = sum(g["size"] * (len(g["files"]) - 1) for g in self.dup_groups)

        def text(value):
            return f"[{utils.fmt_size(value)}]" if value else ""

        mapping = {
            "clean": text(clean_total), "frag": "", "privacy": text(privacy_total),
            "big": text(big_total), "dup": text(dup_total),
            "wechat": text(wechat_total), "qq": text(qq_total), "shred": "",
        }
        for key, value in mapping.items():
            item = self.sidebar_items.get(key)
            if item:
                item.set_size(value)

    def _im_total(self, key):
        data = self.im_scan.get(key)
        if not data:
            return 0
        return sum(info.get("size", 0) for info in data.values())

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
        for key, label in (("rate", "碎片率"), ("space", "碎片空间"), ("free", "可用空间")):
            box = tk.Frame(stats, bg=WHITE)
            box.pack(side="left", expand=True, anchor="w")
            tk.Label(box, text=label, bg=WHITE, fg=MUTED,
                     font=("Microsoft YaHei UI", 10)).pack(anchor="w")
            value = tk.Label(box, text="—", bg=WHITE, fg=ORANGE,
                             font=("Microsoft YaHei UI", 20, "bold"))
            value.pack(anchor="w", pady=(2, 0))
            self.frag_values[key] = value
        grid_canvas = tk.Canvas(parent, bg=WHITE, highlightthickness=0, height=280)
        grid_canvas.pack(fill="both", expand=True, padx=30, pady=(6, 10))
        grid_canvas.bind("<Configure>", lambda _e: self._draw_frag_grid(grid_canvas))

    def _update_frag_stats(self):
        if self._frag_running:
            return
        self._frag_running = True
        drive = self.frag_drive.get()
        self.frag_status.configure(text=f"正在分析 {drive} 的碎片…")

        def work():
            import re
            exe = os.path.join(os.environ.get("SystemRoot", r"C:\Windows"),
                               "System32", "defrag.exe")
            pct = space = free = None
            try:
                proc = subprocess.run([exe, drive, "/A"], capture_output=True,
                                      creationflags=0x08000000, timeout=600)
                out = (proc.stdout or b"").decode("mbcs", "replace")
                match = re.search(r"=\s*(\d+)%", out)
                if match:
                    pct = match.group(1)
                gbs = re.findall(r"=\s*([\d.]+)\s*GB", out)
                if len(gbs) >= 2:
                    free = gbs[1]
                if len(gbs) >= 3:
                    space = gbs[2]
            except Exception:
                pass
            self.queue.put(("frag_stats", pct, space, free))

        threading.Thread(target=work, daemon=True).start()

    def _draw_frag_grid(self, canvas):
        canvas.delete("all")
        width = canvas.winfo_width()
        if width < 40:
            return
        cols = 32
        cell = max(8, (width - 10) // cols)
        colors = ["#bcd9f5", "#a9cef2", "#7fb8ef", "#4aa3ef", "#2f9bf0", "#20c997"]
        for r in range(11):
            for c in range(cols):
                if (r * 7 + c * 3) % 23 == 0:
                    color = colors[(r + c) % len(colors)]
                else:
                    color = "#c3ddf6"
                x = c * cell + 4
                y = r * cell + 4
                canvas.create_rectangle(x, y, x + cell - 3, y + cell - 3,
                                        fill=color, outline="")

    # ---------- 页面：隐私清理 ----------
    def _page_privacy(self, parent):
        self._header_row(parent, [("隐私记录", TEXT, True, 18)], "一键清理", self._privacy_clean)
        self.privacy_sub = tk.Label(parent, text="清理后防止泄露您的小秘密", bg=WHITE,
                                    fg=MUTED, font=("Microsoft YaHei UI", 9))
        self.privacy_sub.pack(anchor="w", padx=18)
        self.privacy_body = self._scrollable(parent)
        self.reg_counts = {k: registry.group_count(k) for k, _ in registry.GROUPS}
        self._refresh_privacy_cards()

    def _refresh_privacy_cards(self):
        for child in self.privacy_body.winfo_children():
            child.destroy()
        self._section_title(self.privacy_body, "高危险隐私", "容易泄露的个人敏感信息")
        privacy = [c for c in self.categories if c.key in ("privacy_browser", "privacy_recent")]
        self._card_grid(self.privacy_body, privacy, self.selected, self._toggle_clean)
        self._section_title(self.privacy_body, "电脑使用痕迹", "使用电脑产生的注册表痕迹")
        self._registry_grid(self.privacy_body)

    def _registry_grid(self, parent):
        grid = tk.Frame(parent, bg=WHITE)
        grid.pack(fill="x", padx=14, pady=(6, 10))
        colors = ["#2980b9", "#27ae60", "#8e44ad", "#d35400"]
        for index, (key, name) in enumerate(registry.GROUPS):
            count = self.reg_counts.get(key, 0)
            card = tk.Frame(grid, bg=WHITE, highlightbackground=BORDER, highlightthickness=1,
                            width=124, height=110)
            card.grid(row=0, column=index, padx=6, pady=4)
            card.pack_propagate(False)
            top = tk.Frame(card, bg=WHITE)
            top.pack(fill="x", padx=6, pady=(6, 2))
            var = tk.BooleanVar(value=key in self.reg_selected)
            self._card_vars.append(var)
            tk.Checkbutton(top, variable=var, bg=WHITE, activebackground=WHITE,
                           highlightthickness=0, bd=0,
                           command=lambda k=key, v=var: self._toggle_reg(k, v.get())).pack(side="left")
            IconTile(top, colors[index % len(colors)], name[:1], 28).pack(side="left", padx=(2, 0))
            tk.Label(card, text=name, bg=WHITE, fg=TEXT, font=("Microsoft YaHei UI", 8),
                     wraplength=112, justify="center").pack(pady=(2, 0))
            Pill(card, f"{count} 项", ORANGE).pack(pady=(3, 5))

    def _toggle_reg(self, key, value):
        if value:
            self.reg_selected.add(key)
        else:
            self.reg_selected.discard(key)

    def _privacy_clean(self):
        if self._cleaning:
            return
        chosen = [c for c in self.categories if c.key in ("privacy_browser", "privacy_recent")]
        groups = set(self.reg_selected)
        if not chosen and not groups:
            messagebox.showinfo("提示", "没有可清理的项目。")
            return
        if not messagebox.askyesno("确认清理",
                                   "将清理浏览器痕迹和注册表使用记录，不可恢复。确定继续？",
                                   icon="warning"):
            return
        self._cleaning = True

        def work():
            freed = 0
            for cat in chosen:
                try:
                    freed += cleaner.clean_category(cat).freed
                except Exception:
                    pass
            deleted = 0
            for group in groups:
                try:
                    deleted += registry.group_clean(group)
                except Exception:
                    pass
            self._cleaning = False
            self.queue.put(("privacy_done", freed, deleted))

        threading.Thread(target=work, daemon=True).start()

    # ---------- 页面：大文件专清 ----------
    def _page_big(self, parent):
        self.big_head = self._header_row(
            parent, [("正在扫描大文件", TEXT, True, 17)], "一键清理", self._delete_big)
        self.big_sub = tk.Label(parent, text="扫描所有磁盘中的大文件（>100 MB）", bg=WHITE,
                                fg=MUTED, font=("Microsoft YaHei UI", 9))
        self.big_sub.pack(anchor="w", padx=18)

        self.big_scope = tk.StringVar(value="全部磁盘")
        self._drive_tabs(parent, ["全部磁盘"] + utils.fixed_drives(),
                         self.big_scope, self._rescan_big)

        self.big_tree = self._make_tree(parent, [
            ("check", "", 36, "center"), ("name", "文件名称", 520, "w"),
            ("disk", "磁盘", 70, "center"), ("size", "文件大小", 100, "e"),
        ])
        self.big_tree.bind("<Button-1>", self._on_big_click)

    def _rescan_big(self):
        self.big_rows = []
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
                rows += scanner.scan_large_files(drive, min_size=100 * 1024 * 1024, limit=500)
            except Exception:
                continue
        rows.sort(key=lambda item: item[1], reverse=True)
        system_names = {"pagefile.sys", "hiberfil.sys", "swapfile.sys",
                        "dumpstack.log.tmp", "memory.dmp"}
        rows = [r for r in rows if os.path.basename(r[0]).lower() not in system_names]
        self._big_running = False
        self.queue.put(("big_rows", rows[:300]))

    def _render_big(self):
        self.big_tree.delete(*self.big_tree.get_children())
        self.big_checked.clear()
        total = 0
        for index, (path, size) in enumerate(self.big_rows):
            total += size
            disk = os.path.splitdrive(path)[0] or "C:"
            self.big_tree.insert("", "end", iid=str(index),
                                 values=("☐", path, disk, utils.fmt_size(size)),
                                 tags=("alt",) if index % 2 else ())
        self._refresh_big_head(total)

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
        if not item or column != "#1":
            return
        if item in self.big_checked:
            self.big_checked.discard(item)
            mark = "☐"
        else:
            self.big_checked.add(item)
            mark = "☑"
        values = list(self.big_tree.item(item, "values"))
        values[0] = mark
        self.big_tree.item(item, values=values)

    def _delete_big(self):
        if not self.big_checked:
            messagebox.showinfo("提示", "请先勾选要删除的大文件。")
            return
        if not messagebox.askyesno("确认", f"将永久删除 {len(self.big_checked)} 个文件，确定？",
                                   icon="warning"):
            return
        freed = 0
        for iid in list(self.big_checked):
            path = self.big_rows[int(iid)][0]
            try:
                size = os.path.getsize(path)
                os.remove(path)
                freed += size
            except OSError:
                pass
        self.start_big_scan()

    # ---------- 页面：重复文件专清 ----------
    def _page_dup(self, parent):
        self.dup_head = self._header_row(
            parent, [("正在查找重复文件", TEXT, True, 17)], "一键清理", self._delete_dup)
        self.dup_sub = tk.Label(parent, text="按内容比对，找出完全相同的文件（全盘范围较大时较慢）",
                                bg=WHITE, fg=MUTED, font=("Microsoft YaHei UI", 9))
        self.dup_sub.pack(anchor="w", padx=18)

        self.dup_scope = tk.StringVar(value="全部磁盘")
        self._drive_tabs(parent, ["全部磁盘", "用户目录"] + utils.fixed_drives(),
                         self.dup_scope, self._rescan_dup)

        self.dup_tree = self._make_tree(parent, [
            ("check", "", 36, "center"), ("name", "文件名称", 600, "w"),
            ("size", "文件大小", 120, "e"),
        ])
        self.dup_tree.bind("<Button-1>", self._on_dup_click)

    def _rescan_dup(self):
        self.dup_groups = []
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

    def _render_dup(self):
        self.dup_tree.delete(*self.dup_tree.get_children())
        self.dup_checked.clear()
        total = 0
        row = 0
        for group in self.dup_groups:
            total += group["size"] * (len(group["files"]) - 1)
            for f_index, path in enumerate(group["files"]):
                iid = f"{row}"
                check = "☐"
                self.dup_tree.insert("", "end", iid=iid,
                                     values=(check, path, utils.fmt_size(group["size"])),
                                     tags=("alt",) if f_index % 2 else ())
                row += 1
        self._refresh_dup_head(total)

    def _refresh_dup_head(self, total):
        for child in self.dup_head.winfo_children():
            child.destroy()
        left = tk.Frame(self.dup_head, bg=WHITE)
        left.pack(side="left", fill="x", expand=True)
        self._headline(left, [
            ("发现 ", TEXT, False, 17), (str(len(self.dup_groups)), RED, True, 21),
            (" 组重复文件，共 ", TEXT, False, 17), (utils.fmt_size(total), RED, True, 21),
        ]).pack(anchor="w")
        GreenButton(self.dup_head, "一键清理", self._delete_dup).pack(side="right")

    def _on_dup_click(self, event):
        item = self.dup_tree.identify_row(event.y)
        column = self.dup_tree.identify_column(event.x)
        if not item or column != "#1":
            return
        if item in self.dup_checked:
            self.dup_checked.discard(item)
            mark = "☐"
        else:
            self.dup_checked.add(item)
            mark = "☑"
        values = list(self.dup_tree.item(item, "values"))
        values[0] = mark
        self.dup_tree.item(item, values=values)

    def _delete_dup(self):
        if not self.dup_checked:
            messagebox.showinfo("提示", "请先勾选要删除的重复文件。")
            return
        if not messagebox.askyesno("确认", f"将永久删除 {len(self.dup_checked)} 个文件，确定？",
                                   icon="warning"):
            return
        for iid in list(self.dup_checked):
            path = self.dup_tree.item(iid, "values")[1]
            try:
                os.remove(path)
            except OSError:
                pass
        self.start_dup_scan()

    # ---------- 页面：微信 / QQ 专清 ----------
    def _page_wechat(self, parent):
        self._im_page(parent, "wechat", "微信文件")

    def _page_qq(self, parent):
        self._im_page(parent, "qq", "QQ 文件")

    def _im_page(self, parent, key, title):
        self._header_row(parent, [(title, TEXT, True, 18)], "一键清理",
                         lambda k=key: self._clean_im(k))
        sub = tk.Label(parent, text="按类型清理，只删媒体文件，不动聊天数据库", bg=WHITE,
                       fg=MUTED, font=("Microsoft YaHei UI", 9))
        sub.pack(anchor="w", padx=18)
        self.im_subs[key] = sub
        body = self._scrollable(parent)
        self.im_bodies[key] = body
        self._refresh_im(key)

    def _refresh_im(self, key):
        body = self.im_bodies.get(key)
        if not body:
            return
        for child in body.winfo_children():
            child.destroy()
        data = self.im_scan.get(key)
        show = data or {}
        total = sum(info.get("size", 0) for info in show.values())
        sub = self.im_subs.get(key)
        if sub:
            if data is None:
                sub.configure(text="正在扫描媒体文件…")
            elif total == 0:
                sub.configure(text="未检测到数据（可能未安装，或没有媒体文件）")
            else:
                sub.configure(text="按类型清理，只删媒体文件，不动聊天数据库")
        grid = tk.Frame(body, bg=WHITE)
        grid.pack(fill="x", padx=14, pady=(8, 10))
        colors = {"image": "#e67e22", "video": "#8e44ad", "audio": "#16a085",
                  "file": "#2f80ed", "cache": "#4b6cb7", "log": "#7f8c8d"}
        for index, (kind, name) in enumerate(imclean.KINDS):
            info = show.get(kind, {"size": 0, "count": 0})
            card = tk.Frame(grid, bg=WHITE, highlightbackground=BORDER, highlightthickness=1,
                            width=124, height=110)
            card.grid(row=0, column=index, padx=6, pady=4)
            card.pack_propagate(False)
            top = tk.Frame(card, bg=WHITE)
            top.pack(fill="x", padx=6, pady=(6, 2))
            var = tk.BooleanVar(value=kind in self.im_selected[key])
            self._card_vars.append(var)
            tk.Checkbutton(top, variable=var, bg=WHITE, activebackground=WHITE,
                           highlightthickness=0, bd=0,
                           command=lambda k=key, kd=kind, v=var: self._toggle_im(k, kd, v.get())).pack(side="left")
            IconTile(top, colors.get(kind, BLUE), name[:1], 28).pack(side="left", padx=(2, 0))
            tk.Label(card, text=name, bg=WHITE, fg=TEXT,
                     font=("Microsoft YaHei UI", 9)).pack(pady=(2, 0))
            Pill(card, utils.fmt_size(info.get("size", 0)), ORANGE).pack(pady=(3, 5))

    def _toggle_im(self, key, kind, value):
        if value:
            self.im_selected[key].add(kind)
        else:
            self.im_selected[key].discard(kind)

    def _scan_im(self, key):
        if self._im_running.get(key):
            return
        self._im_running[key] = True
        sub = self.im_subs.get(key)
        if sub:
            sub.configure(text="正在扫描媒体文件…")

        def work():
            try:
                result = imclean.scan(self.im_roots[key]())
            except Exception:
                result = {}
            self._im_running[key] = False
            self.queue.put(("im_scan", key, result))

        threading.Thread(target=work, daemon=True).start()

    def _clean_im(self, key):
        if self._cleaning:
            return
        kinds = set(self.im_selected[key])
        if not kinds:
            messagebox.showinfo("提示", "请先勾选要清理的类型。")
            return
        if not messagebox.askyesno("确认清理", "将清理所选类型的媒体文件，不可恢复。确定继续？",
                                   icon="warning"):
            return
        self._cleaning = True
        roots = self.im_roots[key]()

        def work():
            try:
                freed, deleted = imclean.clean(roots, kinds)
            except Exception:
                freed, deleted = 0, 0
            self._cleaning = False
            self.queue.put(("im_done", key, freed, deleted))

        threading.Thread(target=work, daemon=True).start()

    # ---------- 页面：文件粉碎 ----------
    def _page_shred(self, parent):
        self._header_row(parent, [("彻底粉碎文件，远离残留", TEXT, True, 17)],
                         "一键粉碎", self._shred_pick)
        tk.Label(parent, text="您可以彻底粉碎无法删除的文件或文件夹，粉碎后将无法恢复",
                 bg=WHITE, fg=MUTED, font=("Microsoft YaHei UI", 9)).pack(anchor="w", padx=18)
        box = tk.Frame(parent, bg=WHITE)
        box.pack(fill="both", expand=True, padx=18, pady=20)
        tk.Label(box, text="＋", bg=WHITE, fg="#9ec5f0",
                 font=("Arial", 48)).pack(pady=(60, 6))
        tk.Label(box, text="点击添加或拖拽需要粉碎的文件/文件夹到这里", bg=WHITE,
                 fg=MUTED, font=("Microsoft YaHei UI", 10)).pack()
        btns = tk.Frame(parent, bg=WHITE)
        btns.pack(fill="x", padx=18, pady=(0, 14))
        for text, cmd in (("添加文件", self._shred_pick_file),
                          ("添加文件夹", self._shred_pick_folder)):
            tk.Label(btns, text=text, bg=BLUE, fg="white", padx=16, pady=6,
                     cursor="hand2", font=("Microsoft YaHei UI", 9, "bold")).pack(
                side="right", padx=(8, 0))
            btns.winfo_children()[-1].bind("<Button-1>", lambda _e, c=cmd: c())

    # ---------- 页面：软件搬家 ----------
    def _page_migrate(self, parent):
        self._header_row(parent, [("软件搬家", TEXT, True, 17)], "刷新列表",
                         self.start_migrate_scan)
        tk.Label(parent, text="把大目录搬到其他盘，原位置自动建立联接，软件照常使用",
                 bg=WHITE, fg=MUTED, font=("Microsoft YaHei UI", 9)).pack(anchor="w", padx=18)
        self.mig_tree = self._make_tree(parent, [
            ("path", "目录", 620, "w"), ("size", "大小", 100, "e"),
            ("status", "状态", 90, "center")])
        dest = tk.Frame(parent, bg=WHITE)
        dest.pack(fill="x", padx=18, pady=(0, 12))
        tk.Label(dest, text="迁移到", bg=WHITE, fg=MUTED,
                 font=("Microsoft YaHei UI", 9)).pack(side="left")
        drives = utils.fixed_drives()
        default = next((d for d in drives if d.upper() != "C:\\"), drives[0])
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
        freed = 0
        for cat in chosen:
            try:
                result = cleaner.clean_category(cat)
                freed += result.freed
            except Exception:
                pass
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

    def _shred_pick(self):
        self._shred_pick_file()

    def _shred_pick_file(self):
        path = filedialog.askopenfilename(title="选择要粉碎的文件")
        if path:
            self._shred(path)

    def _shred_pick_folder(self):
        path = filedialog.askdirectory(title="选择要粉碎的文件夹")
        if path:
            self._shred(path)

    def _shred(self, path):
        if self._cleaning:
            return
        if not messagebox.askyesno("确认粉碎", f"将覆盖写并删除：\n{path}\n\n不可恢复，确定继续？",
                                   icon="warning"):
            return
        self._cleaning = True
        threading.Thread(target=self._shred_worker, args=(path,), daemon=True).start()

    def _shred_worker(self, path):
        try:
            count = shredder.shred_path(path)
        except Exception:
            count = 0
        self._cleaning = False
        self.queue.put(("shred_done", count))

    def start_migrate_scan(self):
        if self._mig_running:
            return
        self._mig_running = True
        threading.Thread(target=self._mig_scan_worker, daemon=True).start()

    def _mig_scan_worker(self):
        rows = []
        for path in migrate.candidate_folders():
            try:
                if migrate.is_junction(path):
                    rows.append((path, 0, "已是联接"))
                elif os.path.isdir(path):
                    rows.append((path, migrate.dir_size(path), "可迁移"))
            except Exception:
                continue
        self._mig_running = False
        self.queue.put(("mig_rows", rows))

    def _render_mig(self, rows):
        self.mig_rows = rows
        self.mig_tree.delete(*self.mig_tree.get_children())
        for index, (path, size, status) in enumerate(rows):
            self.mig_tree.insert("", "end", iid=str(index),
                                 values=(path, utils.fmt_size(size) if size else "—", status),
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
        src, _, status = row
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
        src, _, status = row
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
                self._handle(self.queue.get_nowait())
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
            self._render_dup()
            self._update_sidebar_sizes()
        elif kind == "defrag_done":
            self.worker = None
            self._update_frag_stats()
            messagebox.showinfo("完成", "磁盘优化已结束。")
        elif kind == "shred_done":
            self.worker = None
            messagebox.showinfo("完成", f"已粉碎 {message[1]} 个文件。")
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
        elif kind == "im_scan":
            _, key, result = message
            self.im_scan[key] = result
            self._refresh_im(key)
            self._update_sidebar_sizes()
        elif kind == "frag_stats":
            _, pct, space, free = message
            self._frag_running = False
            if pct:
                self.frag_values["rate"].configure(text=f"{pct}%")
            if space:
                self.frag_values["space"].configure(text=f"{space} GB")
            if free:
                self.frag_values["free"].configure(text=f"{free} GB")
            self.frag_status.configure(text="状态：分析完成")
        elif kind == "im_done":
            _, key, freed, deleted = message
            self.worker = None
            messagebox.showinfo("完成", f"清理 {deleted} 个文件，释放 {utils.fmt_size(freed)}")
            self.im_scan[key] = None
            self._scan_im(key)
        elif kind == "rules_done":
            _, added, total, err = message
            if err:
                messagebox.showinfo("规则更新", err)
            else:
                messagebox.showinfo("规则更新", f"新增 {added} 条，共 {total} 条。重启工具后生效。")


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
