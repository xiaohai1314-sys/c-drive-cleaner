/* 磁盘清理助手 —— Tauri 前端。扫描逻辑在 Rust 侧，这里只负责界面。 */
const invoke = window.__TAURI__?.core?.invoke;
const appWindow = window.__TAURI__?.window?.getCurrentWindow?.();

const SIDEBAR = [
  { key: "clean", label: "C盘瘦身", symbol: "C", color: "#2f80ed" },
  { key: "frag", label: "碎片清理", symbol: "碎", color: "#16a085" },
  { key: "privacy", label: "隐私清理", symbol: "隐", color: "#8e44ad" },
  { key: "big", label: "大文件专清", symbol: "大", color: "#e67e22" },
  { key: "dup", label: "重复文件专清", symbol: "重", color: "#4b6cb7" },
  { key: "wechat", label: "微信专清", symbol: "微", color: "#07c160" },
  { key: "qq", label: "QQ专清", symbol: "Q", color: "#12b7f5" },
  { key: "shred", label: "文件粉碎", symbol: "粉", color: "#e74c3c" },
];

const BIG_KINDS = ["all", "video", "music", "document", "image", "archive", "other"];
const BIG_NAMES = { all: "全部", video: "视频", music: "音乐", document: "文档",
                    image: "图片", archive: "压缩包", other: "其他" };

const state = {
  page: "clean",
  disk: null,
  categories: [],
  selected: new Set(),
  scanningClean: false,
  big: null,
  bigLoading: false,
  bigTab: "all",
  bigChecked: new Set(),
  dup: null,
  dupLoading: false,
  dupScope: "C:\\",
  dupChecked: new Set(),
  drives: [],
  frag: null,
  fragLoading: false,
  fragDrive: "C:\\",
  im: { wechat: null, qq: null },
  imLoading: { wechat: false, qq: false },
  imTab: { wechat: "video", qq: "video" },
  imChecked: { wechat: new Set(), qq: new Set() },
  privacy: null,
  privacyLoading: false,
  privacyChecked: new Set(),
  shredPaths: [],
  scanStatus: null,
  scanPoll: null,
};

const IM_KINDS = {
  wechat: [["video", "聊天视频"], ["image", "聊天图片"], ["file", "接收文件"],
           ["cache", "缓存"], ["log", "日志"], ["sns", "朋友圈"]],
  qq: [["video", "聊天视频"], ["image", "聊天图片"], ["cache", "缓存"],
       ["audio", "聊天语音"], ["file", "接收文件"]],
};

/* ---------- 工具 ---------- */
const $ = (sel) => document.querySelector(sel);

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text != null) node.textContent = text;
  return node;
}

function fmtSize(bytes) {
  const num = Number(bytes) || 0;
  if (num < 1024) return num + " B";
  let value = num;
  for (const unit of ["KB", "MB", "GB", "TB"]) {
    value /= 1024;
    if (value < 1024 || unit === "TB") return value.toFixed(1) + " " + unit;
  }
  return num + " B";
}

function fmtTime(seconds) {
  if (!seconds) return "—";
  const date = new Date(seconds * 1000);
  const pad = (n) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ` +
         `${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

function extOf(path) {
  const name = String(path).split(/[\\/]/).pop() || "";
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot).toLowerCase() : "";
}

const VIDEO_EXT = new Set([".mp4", ".avi", ".mov", ".mkv", ".wmv", ".flv", ".m4v", ".3gp",
  ".mpeg", ".mpg", ".ts", ".webm", ".rmvb", ".vob"]);
const MUSIC_EXT = new Set([".mp3", ".m4a", ".wav", ".flac", ".aac", ".ogg", ".wma", ".ape", ".mid"]);
const IMAGE_EXT = new Set([".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp", ".heic", ".tif",
  ".tiff", ".ico", ".raw", ".psd", ".svg"]);
const DOC_EXT = new Set([".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".txt",
  ".csv", ".rtf", ".ofd", ".md", ".wps", ".et", ".dps", ".dwg", ".dxf"]);
const ARCHIVE_EXT = new Set([".zip", ".rar", ".7z", ".tar", ".gz", ".bz2", ".xz", ".iso", ".cab"]);

function kindOf(path) {
  const ext = extOf(path);
  if (VIDEO_EXT.has(ext)) return "video";
  if (MUSIC_EXT.has(ext)) return "music";
  if (IMAGE_EXT.has(ext)) return "image";
  if (DOC_EXT.has(ext)) return "document";
  if (ARCHIVE_EXT.has(ext)) return "archive";
  return "other";
}

function diskOf(path) {
  const text = String(path);
  return text.length >= 2 && text[1] === ":" ? text.slice(0, 2).toUpperCase() : "";
}

function toast(text) {
  let node = $(".toast");
  if (!node) {
    node = el("div", "toast");
    document.body.appendChild(node);
  }
  node.textContent = text;
  node.classList.add("show");
  clearTimeout(node._timer);
  node._timer = setTimeout(() => node.classList.remove("show"), 2200);
}

function startScanStatus() {
  clearInterval(state.scanPoll);
  state.scanPoll = setInterval(async () => {
    try {
      state.scanStatus = await invoke("scan_status");
      if (state.page === "big") renderBig();
      if (state.page === "dup") renderDup();
    } catch (_) { /* 非 Tauri 预览模式忽略 */ }
  }, 500);
}

function stopScanStatus() {
  clearInterval(state.scanPoll);
  state.scanPoll = null;
  state.scanStatus = null;
}

async function cancelActiveScan(kind) {
  try {
    await invoke("cancel_scan", { kind });
  } catch (error) {
    toast("取消失败：" + error);
  }
}

/* ---------- 侧栏 ---------- */
function renderNav() {
  const nav = $("#nav");
  nav.innerHTML = "";
  for (const item of SIDEBAR) {
    const node = el("div", "nav-item" + (state.page === item.key ? " active" : ""));
    const icon = el("div", "nav-icon", item.symbol);
    icon.style.background = item.color;
    node.appendChild(icon);
    node.appendChild(el("span", null, item.label));
    const size = el("span", "nav-size");
    size.id = "nav-size-" + item.key;
    node.appendChild(size);
    node.addEventListener("click", () => showPage(item.key));
    nav.appendChild(node);
  }
  updateNavSizes();
}

function updateNavSizes() {
  const total = state.categories
    .filter((cat) => state.selected.has(cat.key))
    .reduce((sum, cat) => sum + Math.max(cat.size, 0), 0);
  const set = (key, text) => {
    const node = document.getElementById("nav-size-" + key);
    if (node) node.textContent = text;
  };
  set("clean", total > 0 ? `[${fmtSize(total)}]` : "");
  if (state.big) {
    const size = state.big.reduce((sum, row) => sum + row.size, 0);
    set("big", size > 0 ? `[${fmtSize(size)}]` : "");
  }
  if (state.dup) {
    const size = state.dup.reduce((sum, g) => sum + g.size * Math.max(g.files.length - 1, 0), 0);
    set("dup", size > 0 ? `[${fmtSize(size)}]` : "");
  }
}

function showPage(key) {
  state.page = key;
  renderNav();
  const page = $("#page");
  page.innerHTML = "";
  if (key === "clean") renderClean();
  else if (key === "frag") { renderFrag(); if (!state.frag && !state.fragLoading) analyzeFrag(); }
  else if (key === "big") { renderBig(); if (!state.big && !state.bigLoading) scanBig(); }
  else if (key === "dup") { renderDup(); if (!state.dup && !state.dupLoading) scanDup(); }
  else if (key === "wechat" || key === "qq") {
    renderIm(key);
    if (!state.im[key] && !state.imLoading[key]) scanIm(key);
  }
  else if (key === "privacy") {
    renderPrivacy();
    if (!state.privacy && !state.privacyLoading) scanPrivacy();
  }
  else if (key === "shred") renderShred();
  else renderPlaceholder(key);
}

function renderPlaceholder(key) {
  const item = SIDEBAR.find((one) => one.key === key);
  const box = el("div", "placeholder");
  box.appendChild(el("h2", null, item ? item.label : key));
  box.appendChild(el("div", null, "这个页面还在从 Python 版迁移到 Rust，后续会接上。"));
  $("#page").appendChild(box);
}

/* ---------- 首页：C盘瘦身 ---------- */
function renderClean() {
  const page = $("#page");
  page.innerHTML = "";

  const found = state.categories.reduce((sum, cat) => sum + Math.max(cat.size, 0), 0);
  const chosen = state.categories
    .filter((cat) => state.selected.has(cat.key))
    .reduce((sum, cat) => sum + Math.max(cat.size, 0), 0);

  const head = el("div", "page-head");
  const headline = el("div", "headline");
  headline.append("发现 ", el("span", "num", fmtSize(found)), " 可释放，已勾选 ",
                  el("span", "num", fmtSize(chosen)));
  head.appendChild(headline);
  const btn = el("button", "green-btn", "一键瘦身");
  btn.addEventListener("click", () => cleanSelected(btn));
  head.appendChild(btn);
  page.appendChild(head);

  page.appendChild(el("div", "sub",
    state.scanningClean ? "正在统计各类缓存…" : `已选 ${state.selected.size} 项`));

  const row = el("div", "disk-row");
  const bar = el("div", "disk-bar");
  const fill = el("div", "disk-fill");
  const ratio = state.disk && state.disk.total ? state.disk.used / state.disk.total : 0;
  fill.style.width = Math.max(3, Math.round(ratio * 100)) + "%";
  bar.appendChild(fill);
  row.appendChild(bar);
  row.appendChild(el("span", "disk-text", state.disk
    ? `C: 共${fmtSize(state.disk.total)}，已用${fmtSize(state.disk.used)}`
    : "正在读取磁盘信息…"));
  page.appendChild(row);

  const recommended = state.categories.filter((cat) => cat.implemented !== false && !cat.requires_confirm && cat.tier !== "deep");
  const confirm = state.categories.filter((cat) => cat.requires_confirm || cat.tier === "deep");
  for (const [title, note, list] of [
    ["推荐清理项", "系统和软件的垃圾和无用文件，可放心清理", recommended],
    ["谨慎清理项", "含需确认的数据及不可恢复日志，勾选前请查看提示", confirm],
  ]) {
    if (!list.length) continue;
    const section = el("div", "section");
    const titleRow = el("div", "section-title");
    titleRow.appendChild(el("span", null, title));
    titleRow.appendChild(el("span", "note", note));
    const sizes = el("span", "sizes",
      `已选${fmtSize(list.filter((c) => state.selected.has(c.key)).reduce((s, c) => s + Math.max(c.size, 0), 0))}` +
      `/共${fmtSize(list.reduce((s, c) => s + Math.max(c.size, 0), 0))}`);
    titleRow.appendChild(sizes);
    section.appendChild(titleRow);

    const grid = el("div", "cards");
    for (const cat of list) {
      const checked = state.selected.has(cat.key);
      const unsupported = cat.implemented === false;
      const card = el("div", "card" + (checked ? " checked" : "") + (unsupported ? " unsupported" : ""));
      const icon = el("div", "card-icon", (cat.name || cat.key).slice(0, 1));
      icon.style.background = colorFor(cat.key);
      card.appendChild(icon);
      card.appendChild(el("div", "card-name", cat.name));
      card.appendChild(el("div", "card-size", cat.size > 0 ? fmtSize(cat.size) : "—"));
      card.appendChild(el("div", "card-desc", cat.describe || ""));
      card.appendChild(el("div", "tick"));
      card.title = unsupported ? "该清理项需要系统命令，当前版本暂未迁移。" : (cat.describe || "") + (cat.warning ? "\n\n注意：" + cat.warning : "");
      card.addEventListener("click", () => {
        if (unsupported) { toast("该项目暂未支持清理"); return; }
        if (state.selected.has(cat.key)) state.selected.delete(cat.key);
        else state.selected.add(cat.key);
        renderClean();
        updateNavSizes();
      });
      grid.appendChild(card);
    }
    section.appendChild(grid);
    page.appendChild(section);
  }

  if (!state.categories.length && !state.scanningClean) {
    page.appendChild(el("div", "empty", "没有扫描到可清理项。"));
  }
}

const PALETTE = ["#2f80ed", "#16a085", "#8e44ad", "#e67e22", "#4b6cb7", "#07c160",
  "#12b7f5", "#e74c3c", "#d35400", "#2980b9"];
function colorFor(key) {
  let sum = 0;
  for (const ch of String(key)) sum += ch.charCodeAt(0);
  return PALETTE[sum % PALETTE.length];
}

async function scanCategories() {
  state.scanningClean = true;
  renderClean();
  try {
    state.categories = await invoke("scan_categories");
  } catch (error) {
    state.categories = [];
    toast("扫描失败：" + error);
  }
  state.scanningClean = false;
  // 默认勾选推荐项，和 Python 版一致。
  for (const cat of state.categories) {
    if (cat.implemented !== false && !cat.requires_confirm && cat.tier !== "deep") state.selected.add(cat.key);
  }
  renderClean();
  updateNavSizes();
}

/* ---------- 碎片清理 ---------- */
function renderFrag() {
  const page = $("#page");
  page.innerHTML = "";

  const report = state.frag;
  const head = el("div", "page-head");
  head.appendChild(el("div", "headline", "清理系统碎片，加速电脑告别卡顿"));
  const btn = el("button", "green-btn", "一键清理");
  btn.addEventListener("click", () => optimizeFrag(btn));
  head.appendChild(btn);
  page.appendChild(head);

  let status = `状态：正在分析 ${state.fragDrive} …`;
  if (!state.fragLoading) {
    if (!report) {
      status = "状态：准备分析…";
    } else if (report.ssd === true) {
      status = `状态：${state.fragDrive.slice(0, 2)} 是固态硬盘，不需要整理碎片。「一键清理」只会让系统做 TRIM 优化`;
    } else if (report.percent == null && report.count == null) {
      status = "状态：没能读到碎片分析。下方格子是示意，不是磁盘簇的真实布局";
    } else {
      status = `状态：${state.fragDrive.slice(0, 2)} 分析完成。分布图为示意，按碎片率上色`;
    }
  }
  page.appendChild(el("div", "sub", status));

  const tabs = el("div", "tabs");
  const drives = state.drives.length ? state.drives : ["C:\\"];
  for (const drive of drives) {
    const tab = el("button", "tab" + (state.fragDrive === drive ? " active" : ""));
    tab.textContent = drive.slice(0, 1) + "盘";
    tab.addEventListener("click", () => {
      if (state.fragDrive === drive) return;
      state.fragDrive = drive;
      state.frag = null;
      renderFrag();
      analyzeFrag();
    });
    tabs.appendChild(tab);
  }
  page.appendChild(tabs);

  const stats = el("div", "frag-stats");
  const items = [
    ["碎片数", report && report.count != null ? String(report.count) : "—"],
    ["碎片文件", report && report.space_gb != null ? report.space_gb.toFixed(2) + " GB" : "—"],
    ["碎片率", report && report.percent != null ? report.percent + "%" : "—"],
  ];
  for (const [label, value] of items) {
    const box = el("div", "frag-stat");
    box.appendChild(el("div", "frag-label", label));
    box.appendChild(el("div", "frag-value", value));
    stats.appendChild(box);
  }
  page.appendChild(stats);

  const rows = 11;
  const cols = 32;
  const total = rows * cols;
  const hot = hotCells(report ? report.percent : null, total);
  const grid = el("div", "frag-grid");
  for (let index = 0; index < total; index += 1) {
    grid.appendChild(el("div", "frag-cell" + (hot.has(index) ? " hot" : "")));
  }
  page.appendChild(grid);
  page.appendChild(el("div", "frag-note", "示意（按碎片率上色，不是磁盘簇的真实布局）"));
}

function hotCells(percent, total) {
  const hot = new Set();
  if (percent == null) return hot;
  const rate = Math.max(0, Math.min(100, Number(percent)));
  const count = Math.round(total * rate / 100);
  if (count >= total) {
    for (let index = 0; index < total; index += 1) hot.add(index);
    return hot;
  }
  const step = total / Math.max(count, 1);
  for (let index = 0; index < count; index += 1) {
    hot.add(Math.min(total - 1, Math.floor(index * step)));
  }
  let cursor = 0;
  while (hot.size < count && cursor < total) {
    hot.add(cursor);
    cursor += 1;
  }
  return hot;
}

async function analyzeFrag() {
  state.fragLoading = true;
  renderFrag();
  try {
    state.frag = await invoke("frag_analyze", { drive: state.fragDrive });
  } catch (error) {
    state.frag = null;
    toast("分析失败：" + error);
  }
  state.fragLoading = false;
  renderFrag();
}

async function optimizeFrag(button) {
  const drive = state.fragDrive;
  const ssd = state.frag && state.frag.ssd === true;
  const lines = ssd
    ? [`${drive.slice(0, 2)} 是固态硬盘，将让系统执行 TRIM 优化，不做碎片整理。`]
    : [`将对 ${drive.slice(0, 2)} 执行碎片整理，可能需要较长时间，期间请不要关闭窗口。`];
  if (!(await showConfirm("确认优化磁盘", lines, ssd ? "开始优化" : "开始整理"))) return;
  button.disabled = true;
  button.textContent = "优化中…";
  try {
    await invoke("frag_optimize", { drive });
    await showResult("优化完成", ["磁盘优化已结束。"]);
  } catch (error) {
    toast("优化失败：" + error);
  }
  button.disabled = false;
  button.textContent = "一键清理";
  state.frag = null;
  await analyzeFrag();
}

/* ---------- 大文件 ---------- */
function renderBig() {
  const page = $("#page");
  page.innerHTML = "";

  const rows = state.big || [];
  const total = rows.reduce((sum, row) => sum + row.size, 0);
  const head = el("div", "page-head");
  const headline = el("div", "headline");
  headline.append("发现 ", el("span", "num", String(rows.length)), " 个大文件，共 ",
                  el("span", "num", fmtSize(total)));
  head.appendChild(headline);
  const btn = el("button", "green-btn", "一键清理");
  btn.addEventListener("click", () => deleteBig(btn));
  head.appendChild(btn);
  if (state.bigLoading) {
    const cancel = el("button", "gray-btn", "取消扫描");
    cancel.addEventListener("click", () => cancelActiveScan("big"));
    head.appendChild(cancel);
  }
  page.appendChild(head);
  page.appendChild(el("div", "sub", state.bigLoading
    ? `正在扫描各磁盘的大文件…${state.scanStatus?.big?.cancelled ? " 正在停止" : ""}`
    : "扫描磁盘中大于 100 MB 的文件，删除前会先预览"));

  const totals = { all: 0 };
  for (const row of rows) {
    totals.all += row.size;
    const kind = kindOf(row.path);
    totals[kind] = (totals[kind] || 0) + row.size;
  }
  const tabs = el("div", "tabs");
  for (const kind of BIG_KINDS) {
    const tab = el("button", "tab" + (state.bigTab === kind ? " active" : ""));
    tab.appendChild(document.createTextNode(BIG_NAMES[kind]));
    if (kind !== "all" && totals[kind]) tab.appendChild(el("span", "tab-size", fmtSize(totals[kind])));
    tab.addEventListener("click", () => { state.bigTab = kind; renderBig(); });
    tabs.appendChild(tab);
  }
  page.appendChild(tabs);

  const visible = state.bigTab === "all" ? rows : rows.filter((row) => kindOf(row.path) === state.bigTab);
  const table = el("div", "table");
  const thead = el("div", "thead");
  thead.style.gridTemplateColumns = "28px 1fr 70px 100px 140px";
  thead.append(el("span", null, ""), el("span", null, "文件名称"), el("span", null, "磁盘"),
               el("span", null, "文件大小"), el("span", null, "修改时间"));
  table.appendChild(thead);

  if (!visible.length) {
    table.appendChild(el("div", "empty", state.bigLoading ? "扫描中…" : "没有找到大文件。"));
  }
  for (const row of visible) {
    const line = el("div", "row");
    line.style.gridTemplateColumns = "28px 1fr 70px 100px 140px";
    const check = el("div", "check" + (state.bigChecked.has(row.path) ? " on" : ""));
    check.addEventListener("click", () => {
      if (state.bigChecked.has(row.path)) state.bigChecked.delete(row.path);
      else state.bigChecked.add(row.path);
      renderBig();
    });
    line.appendChild(check);
    const name = el("div", "name", row.path);
    name.title = row.path;
    line.appendChild(name);
    line.appendChild(el("div", "muted", diskOf(row.path)));
    line.appendChild(el("div", "muted", fmtSize(row.size)));
    line.appendChild(el("div", "muted", fmtTime(row.mtime)));
    table.appendChild(line);
  }
  page.appendChild(table);
}

async function scanBig() {
  state.bigLoading = true;
  startScanStatus();
  renderBig();
  try {
    state.big = await invoke("scan_big");
  } catch (error) {
    state.big = [];
    toast("扫描失败：" + error);
  }
  state.bigLoading = false;
  stopScanStatus();
  renderBig();
  updateNavSizes();
}

/* ---------- 重复文件 ---------- */
function renderDup() {
  const page = $("#page");
  page.innerHTML = "";

  const groups = state.dup || [];
  const reclaim = groups.reduce((sum, g) => sum + g.size * Math.max(g.files.length - 1, 0), 0);
  const head = el("div", "page-head");
  const headline = el("div", "headline");
  headline.append("发现 ", el("span", "num", String(groups.length)), " 组重复文件，共 ",
                  el("span", "num", fmtSize(reclaim)));
  head.appendChild(headline);
  const btn = el("button", "green-btn", "一键清理");
  btn.addEventListener("click", () => deleteDups(btn));
  head.appendChild(btn);
  if (state.dupLoading) {
    const cancel = el("button", "gray-btn", "取消扫描");
    cancel.addEventListener("click", () => cancelActiveScan("dup"));
    head.appendChild(cancel);
  }
  page.appendChild(head);
  page.appendChild(el("div", "sub", state.dupLoading
    ? `正在查找重复文件…${state.scanStatus?.dup?.cancelled ? " 正在停止" : "（默认只扫 C 盘）"}`
    : `每组自动保留一份，已勾选 ${state.dupChecked.size} 个副本`));

  const scopes = [["C:\\", "C盘"], ["USER", "用户目录"], ["ALL", "全部磁盘"]];
  const tabs = el("div", "tabs");
  for (const [value, label] of scopes) {
    const tab = el("button", "tab" + (state.dupScope === value ? " active" : ""));
    tab.textContent = label;
    tab.addEventListener("click", () => {
      if (state.dupScope === value) return;
      state.dupScope = value;
      state.dup = null;
      state.dupChecked = new Set();
      renderDup();
      scanDup();
    });
    tabs.appendChild(tab);
  }
  page.appendChild(tabs);

  if (!groups.length) {
    page.appendChild(el("div", "empty", state.dupLoading ? "扫描中…" : "没有找到重复文件。"));
    return;
  }

  for (const group of groups.slice(0, 300)) {
    const box = el("div", "group");
    const headRow = el("div", "group-head");
    headRow.append(el("b", null, fmtSize(group.size)), document.createTextNode(` · ${group.files.length} 份 · 可省 `),
                   el("b", null, fmtSize(group.size * (group.files.length - 1))));
    box.appendChild(headRow);

    const keeper = pickKeeper(group);
    for (const path of group.files) {
      const line = el("div", "row");
      line.style.gridTemplateColumns = "28px 1fr 100px 140px";
      const isKeeper = path === keeper;
      const checked = state.dupChecked.has(path);
      const check = el("div", "check" + (checked && !isKeeper ? " on" : ""));
      if (isKeeper) check.style.opacity = "0.35";
      else check.addEventListener("click", () => {
        if (state.dupChecked.has(path)) state.dupChecked.delete(path);
        else state.dupChecked.add(path);
        renderDup();
      });
      line.appendChild(check);
      const name = el("div", "name", path);
      name.title = path;
      if (isKeeper) name.appendChild(el("span", "badge", "保留"));
      line.appendChild(name);
      line.appendChild(el("div", "muted", fmtSize(group.size)));
      line.appendChild(el("div", "muted", fmtTime(group.mtimes[group.files.indexOf(path)])));
      box.appendChild(line);
    }
    page.appendChild(box);
  }
  if (groups.length > 300) {
    page.appendChild(el("div", "empty", `只显示了前 300 组，共 ${groups.length} 组。`));
  }
}

function pickKeeper(group) {
  let best = null;
  let bestKey = null;
  group.files.forEach((path, index) => {
    const key = [group.mtimes[index] || 0, -path.length];
    if (!bestKey || key[0] > bestKey[0] || (key[0] === bestKey[0] && key[1] > bestKey[1])) {
      bestKey = key;
      best = path;
    }
  });
  return best;
}

async function scanDup() {
  state.dupLoading = true;
  startScanStatus();
  renderDup();
  try {
    const root = state.dupScope === "C:\\" ? "C:\\"
      : state.dupScope === "ALL" ? null
      : await invoke("user_profile");
    state.dup = await invoke("scan_dups", { root });
    state.dupChecked = new Set();
    for (const group of state.dup) {
      const keeper = pickKeeper(group);
      for (const path of group.files) if (path !== keeper) state.dupChecked.add(path);
    }
  } catch (error) {
    state.dup = [];
    toast("扫描失败：" + error);
  }
  state.dupLoading = false;
  stopScanStatus();
  renderDup();
  updateNavSizes();
}

/* ---------- 微信 / QQ 专清 ---------- */
function renderIm(key) {
  const page = $("#page");
  page.innerHTML = "";
  const files = state.im[key] || [];
  const title = key === "wechat" ? "微信文件" : "QQ 文件";

  const head = el("div", "page-head");
  head.appendChild(el("div", "headline", title));
  const btn = el("button", "green-btn", "一键清理");
  btn.addEventListener("click", () => cleanIm(key, btn));
  head.appendChild(btn);
  page.appendChild(head);

  const chosen = files.filter((file) => state.imChecked[key].has(file.path));
  const chosenSize = chosen.reduce((sum, file) => sum + file.size, 0);
  page.appendChild(el("div", "sub", state.imLoading[key]
    ? "正在扫描聊天文件…"
    : `已勾选 ${chosen.length} 个，共 ${fmtSize(chosenSize)}`));

  const totals = {};
  for (const file of files) {
    const box = totals[file.kind] || (totals[file.kind] = { size: 0, count: 0 });
    box.size += file.size;
    box.count += 1;
  }
  const tabs = el("div", "tabs");
  for (const [kind, label] of IM_KINDS[key]) {
    const tab = el("button", "tab" + (state.imTab[key] === kind ? " active" : ""));
    tab.appendChild(document.createTextNode(label));
    if (totals[kind]) tab.appendChild(el("span", "tab-size", fmtSize(totals[kind].size)));
    tab.addEventListener("click", () => {
      state.imTab[key] = kind;
      renderIm(key);
    });
    tabs.appendChild(tab);
  }
  page.appendChild(tabs);

  const visible = files.filter((file) => file.kind === state.imTab[key]);
  if (!visible.length) {
    page.appendChild(el("div", "empty", state.imLoading[key] ? "扫描中…" : "没有找到这类文件。"));
    return;
  }
  const limit = 2000;
  const shown = visible.slice(0, limit);
  const groups = new Map();
  for (const file of shown) {
    if (!groups.has(file.month)) groups.set(file.month, []);
    groups.get(file.month).push(file);
  }
  for (const [month, items] of groups) {
    const box = el("div", "group");
    const headRow = el("div", "group-head");
    const total = items.reduce((sum, file) => sum + file.size, 0);
    headRow.append(el("b", null, month),
                   document.createTextNode(` · ${items.length} 个 · ${fmtSize(total)}`));
    box.appendChild(headRow);
    for (const file of items) {
      const row = el("div", "row");
      row.style.gridTemplateColumns = "28px 1fr 100px 140px";
      const check = el("div", "check" + (state.imChecked[key].has(file.path) ? " on" : ""));
      check.addEventListener("click", () => {
        if (state.imChecked[key].has(file.path)) state.imChecked[key].delete(file.path);
        else state.imChecked[key].add(file.path);
        check.classList.toggle("on");
        updateImSub(key);
      });
      row.appendChild(check);
      const name = el("div", "name", file.name);
      name.title = file.path;
      row.appendChild(name);
      row.appendChild(el("div", "muted", fmtSize(file.size)));
      row.appendChild(el("div", "muted", fmtTime(file.mtime)));
      box.appendChild(row);
    }
    page.appendChild(box);
  }
  if (visible.length > limit) {
    page.appendChild(el("div", "empty", `只显示了前 ${limit} 个，共 ${visible.length} 个。`));
  }
}

function updateImSub(key) {
  const sub = document.querySelector("#page .sub");
  if (!sub) return;
  const files = state.im[key] || [];
  const chosen = files.filter((file) => state.imChecked[key].has(file.path));
  const size = chosen.reduce((sum, file) => sum + file.size, 0);
  sub.textContent = `已勾选 ${chosen.length} 个，共 ${fmtSize(size)}`;
}

async function scanIm(key) {
  state.imLoading[key] = true;
  renderIm(key);
  try {
    state.im[key] = await invoke("im_scan", { kind: key });
  } catch (error) {
    state.im[key] = [];
    toast("扫描失败：" + error);
  }
  state.imLoading[key] = false;
  state.imChecked[key] = new Set(
    (state.im[key] || [])
      .filter((file) => file.kind === "cache" || file.kind === "log")
      .map((file) => file.path),
  );
  renderIm(key);
}

async function cleanIm(key, button) {
  const files = state.im[key] || [];
  const chosen = files.filter((file) => state.imChecked[key].has(file.path));
  if (!chosen.length) {
    toast("请先勾选要清理的文件");
    return;
  }
  const total = chosen.reduce((sum, file) => sum + file.size, 0);
  const kinds = [...new Set(chosen.map((file) => file.kind))];
  const lines = [`将把 ${chosen.length} 个文件移到回收站，约 ${fmtSize(total)}。`,
                 "聊天数据库、配置和密码类文件不会被清理。"];
  if (!(await showConfirm("确认清理", lines, "移到回收站"))) return;
  button.disabled = true;
  button.textContent = "清理中…";
  try {
    const report = await invoke("im_clean", {
      kind,
      kinds,
      paths: chosen.map((file) => file.path),
    });
    const result = [`移到回收站 ${report.deleted} 个，释放 ${fmtSize(report.freed)}。`];
    for (const reason of (report.reasons || []).slice(0, 5)) result.push(reason);
    await showResult("清理完成", result);
  } catch (error) {
    toast("清理失败：" + error);
  }
  button.disabled = false;
  button.textContent = "一键清理";
  state.im[key] = null;
  state.imChecked[key] = new Set();
  await scanIm(key);
}

/* ---------- 隐私清理 ---------- */
const PRIVACY_SECTIONS = [
  ["high", "高危隐私", "容易泄露的个人敏感信息"],
  ["history", "上网浏览历史", "使用浏览器时产生的痕迹"],
  ["download", "下载历史记录", "浏览器下载记录，不删除已经下载的文件"],
  ["shop", "网络购物记录", "浏览记录里能识别的购物网站"],
  ["computer", "电脑使用痕迹", "使用电脑系统产生的痕迹"],
  ["software", "软件使用痕迹", "程序运行记录"],
  ["media", "影音播放痕迹", "播放器最近播放"],
];

function renderPrivacy() {
  const page = $("#page");
  page.innerHTML = "";
  const items = state.privacy || [];

  const head = el("div", "page-head");
  head.appendChild(el("div", "headline", "隐私记录"));
  const btn = el("button", "green-btn", "一键清理");
  btn.addEventListener("click", () => cleanPrivacy(btn));
  head.appendChild(btn);
  page.appendChild(head);
  page.appendChild(el("div", "sub", state.privacyLoading
    ? "正在扫描浏览器和注册表…"
    : `已选 ${state.privacyChecked.size} 项。已保存的密码不会被删除。`));

  for (const [section, title, note] of PRIVACY_SECTIONS) {
    const list = items.filter((item) => item.section === section);
    if (!list.length) continue;
    const box = el("div", "section");
    const titleRow = el("div", "section-title");
    titleRow.appendChild(el("span", null, title));
    titleRow.appendChild(el("span", "note", note));
    box.appendChild(titleRow);
    const grid = el("div", "cards");
    for (const item of list) {
      const selectable = item.action !== "count"
        && (item.count_available !== false || (item.files && item.files.length > 0));
      const checked = state.privacyChecked.has(item.id);
      const card = el("div", "pcard" + (checked ? " checked" : "") + (selectable ? "" : " disabled"));
      card.appendChild(el("div", "check" + (checked ? " on" : "")));
      const body = el("div", "pbody");
      body.appendChild(el("div", "pname", item.name));
      body.appendChild(el("div", "pcount",
        item.count_available === false ? "—" : `${item.count} 条`));
      if (item.note) body.appendChild(el("div", "pnote", item.note));
      card.appendChild(body);
      if (selectable) {
        card.addEventListener("click", () => {
          if (state.privacyChecked.has(item.id)) state.privacyChecked.delete(item.id);
          else state.privacyChecked.add(item.id);
          renderPrivacy();
        });
      }
      grid.appendChild(card);
    }
    box.appendChild(grid);
    page.appendChild(box);
  }
  if (!items.length && !state.privacyLoading) {
    page.appendChild(el("div", "empty", "没有扫描到隐私痕迹。"));
  }
}

async function scanPrivacy() {
  state.privacyLoading = true;
  renderPrivacy();
  try {
    state.privacy = await invoke("privacy_scan");
  } catch (error) {
    state.privacy = [];
    toast("扫描失败：" + error);
  }
  state.privacyLoading = false;
  state.privacyChecked = new Set(
    (state.privacy || [])
      .filter((item) => item.action !== "count" && item.count > 0 && item.count_available !== false)
      .map((item) => item.id),
  );
  renderPrivacy();
}

async function cleanPrivacy(button) {
  const ids = [...state.privacyChecked];
  if (!ids.length) {
    toast("请先勾选要清理的项目");
    return;
  }
  const lines = [`将清理 ${ids.length} 项隐私痕迹。`,
                 "浏览器历史、Cookie、下载记录会被删除；文件类痕迹移到回收站；注册表项删除前会导出备份。",
                 "已保存的密码不会被删除。"];
  if (!(await showConfirm("确认清理隐私痕迹", lines, "开始清理"))) return;
  button.disabled = true;
  button.textContent = "清理中…";
  try {
    const report = await invoke("privacy_clean", { ids });
    const result = [`文件 ${report.files} 个，数据库 ${report.sql} 个，注册表记录 ${report.registry} 条。`];
    for (const failure of (report.failed || []).slice(0, 5)) result.push(failure);
    await showResult("清理完成", result);
  } catch (error) {
    toast("清理失败：" + error);
  }
  button.disabled = false;
  button.textContent = "一键清理";
  state.privacy = null;
  state.privacyChecked = new Set();
  await scanPrivacy();
}

/* ---------- 文件粉碎 ---------- */
function renderShred() {
  const page = $("#page");
  page.innerHTML = "";

  const head = el("div", "page-head");
  head.appendChild(el("div", "headline", "彻底粉碎文件，远离残留"));
  const btn = el("button", "green-btn", "一键粉碎");
  btn.addEventListener("click", () => runShred(btn));
  head.appendChild(btn);
  page.appendChild(head);
  page.appendChild(el("div", "sub",
    "先加入列表，确认后再覆盖写入并删除。系统目录不能粉碎，粉碎后无法恢复。"));

  const actions = el("div", "shred-actions");
  const addFiles = el("button", "btn btn-ghost", "添加文件");
  addFiles.addEventListener("click", () => pickShred(false));
  const addFolder = el("button", "btn btn-ghost", "添加文件夹");
  addFolder.addEventListener("click", () => pickShred(true));
  actions.appendChild(addFiles);
  actions.appendChild(addFolder);
  page.appendChild(actions);

  if (!state.shredPaths.length) {
    page.appendChild(el("div", "empty", "还没有添加文件。点击上方按钮选择需要粉碎的文件或文件夹。"));
    return;
  }
  const list = el("div", "shred-list");
  for (const path of state.shredPaths) {
    const row = el("div", "shred-row");
    const name = el("div", "name", path);
    name.title = path;
    row.appendChild(name);
    const remove = el("div", "shred-remove", "移除");
    remove.addEventListener("click", () => {
      state.shredPaths = state.shredPaths.filter((item) => item !== path);
      renderShred();
    });
    row.appendChild(remove);
    list.appendChild(row);
  }
  page.appendChild(list);
}

async function pickShred(folder) {
  const dialog = window.__TAURI__ && window.__TAURI__.dialog;
  if (!dialog) {
    toast("文件选择器不可用");
    return;
  }
  try {
    const picked = await dialog.open({
      directory: folder,
      multiple: true,
      title: folder ? "选择要粉碎的文件夹" : "选择要粉碎的文件",
    });
    const list = Array.isArray(picked) ? picked : (picked ? [picked] : []);
    for (const path of list) {
      if (!state.shredPaths.includes(path)) state.shredPaths.push(path);
    }
    renderShred();
  } catch (error) {
    toast("选择失败：" + error);
  }
}

async function runShred(button) {
  if (!state.shredPaths.length) {
    toast("请先添加要粉碎的文件");
    return;
  }
  const lines = [`将粉碎 ${state.shredPaths.length} 个路径。`,
                 "粉碎会覆盖写入后删除，无法恢复，不走回收站。",
                 "系统目录和受保护路径会被拒绝。"];
  if (!(await showConfirm("确认粉碎", lines, "开始粉碎"))) return;
  button.disabled = true;
  button.textContent = "粉碎中…";
  try {
    const report = await invoke("shred", { paths: state.shredPaths, passes: 1 });
    const result = [`成功 ${report.files_ok} 个，失败 ${report.files_failed} 个，释放 ${fmtSize(report.freed)}。`];
    for (const error of (report.errors || []).slice(0, 5)) result.push(error);
    await showResult("粉碎完成", result);
    state.shredPaths = [];
  } catch (error) {
    toast("粉碎失败：" + error);
  }
  button.disabled = false;
  button.textContent = "一键粉碎";
  renderShred();
}

/* ---------- 清理历史 / 规则更新 ---------- */
async function showHistory() {
  let entries = [];
  try {
    entries = await invoke("history_list");
  } catch (error) {
    toast("读取历史失败：" + error);
    return;
  }
  if (!entries.length) {
    await showResult("清理历史", ["还没有清理记录。"]);
    return;
  }
  const mask = el("div", "modal-mask");
  const box = el("div", "modal");
  box.appendChild(el("h3", null, "清理历史"));
  const body = el("div", "modal-body");
  for (const entry of entries.slice(0, 100)) {
    const row = el("div", "history-row");
    row.appendChild(el("span", "history-name", entry.name));
    row.appendChild(el("span", "history-size", fmtSize(entry.size)));
    row.addEventListener("click", async () => {
      try {
        const text = await invoke("history_read", { name: entry.name });
        body.innerHTML = "";
        body.appendChild(el("pre", "history-text", text));
        const back = el("button", "btn btn-ghost", "返回列表");
        back.addEventListener("click", () => {
          mask.remove();
          showHistory();
        });
        body.appendChild(back);
      } catch (error) {
        toast("读取失败：" + error);
      }
    });
    body.appendChild(row);
  }
  box.appendChild(body);
  const actions = el("div", "modal-actions");
  const close = el("button", "btn btn-primary", "关闭");
  close.addEventListener("click", () => mask.remove());
  actions.appendChild(close);
  box.appendChild(actions);
  mask.appendChild(box);
  document.body.appendChild(mask);
}

async function updateRules() {
  try {
    const message = await invoke("rules_update");
    await showResult("规则更新", message.split("\n"));
  } catch (error) {
    await showResult("规则更新", [String(error)]);
  }
}

async function rollbackRules() {
  try {
    const message = await invoke("rules_rollback");
    await showResult("规则回滚", [message, "下次扫描生效。"]);
  } catch (error) {
    await showResult("规则回滚", [String(error)]);
  }
}

/* ---------- 删除执行 ---------- */
async function cleanSelected(button) {
  const chosen = state.categories.filter((cat) => state.selected.has(cat.key));
  if (!chosen.length) {
    toast("请先勾选要清理的项目");
    return;
  }
  const total = chosen.reduce((sum, cat) => sum + Math.max(cat.size, 0), 0);
  const deep = chosen.filter((cat) => cat.requires_confirm);
  const lines = [`将清理 ${chosen.length} 项，预计释放 ${fmtSize(total)}。`,
                 "清理会直接删除文件，不进回收站。"];
  if (deep.length) {
    lines.push(`其中 ${deep.length} 项含需确认的数据：${deep.map((cat) => cat.name).join("、")}。`);
  }
  if (!(await showConfirm("确认清理", lines, "开始清理"))) return;
  button.disabled = true;
  button.textContent = "清理中…";
  try {
    const report = await invoke("clean_categories", { keys: chosen.map((cat) => cat.key) });
    const result = [`释放 ${fmtSize(report.freed)}，删除 ${report.deleted} 个文件。`];
    if (report.pending) result.push(`有 ${report.pending} 个文件被占用，已安排在重启后删除。`);
    if (report.skipped) result.push(`跳过 ${report.skipped} 项（受保护、正在使用或规则不允许）。`);
    for (const note of report.notes || []) result.push(note);
    for (const error of (report.errors || []).slice(0, 5)) result.push(error);
    await showResult("清理完成", result);
  } catch (error) {
    toast("清理失败：" + error);
  }
  button.disabled = false;
  button.textContent = "一键瘦身";
  await scanCategories();
}

async function deleteBig(button) {
  const rows = (state.big || []).filter((row) => state.bigChecked.has(row.path));
  if (!rows.length) {
    toast("请先勾选要清理的大文件");
    return;
  }
  const total = rows.reduce((sum, row) => sum + row.size, 0);
  const lines = [`将把 ${rows.length} 个文件移到回收站，约 ${fmtSize(total)}。`,
                 "移到回收站后可以从回收站恢复。"];
  if (!(await showConfirm("确认清理大文件", lines, "移到回收站"))) return;
  button.disabled = true;
  button.textContent = "清理中…";
  try {
    const report = await invoke("delete_recycle", { paths: rows.map((row) => row.path) });
    const result = [`移到回收站 ${report.deleted} 个，释放 ${fmtSize(report.freed)}。`];
    for (const reason of (report.reasons || []).slice(0, 5)) result.push(reason);
    await showResult("清理完成", result);
    state.bigChecked = new Set(report.failed || []);
  } catch (error) {
    toast("清理失败：" + error);
  }
  button.disabled = false;
  button.textContent = "一键清理";
  await scanBig();
}

async function deleteDups(button) {
  const items = [];
  for (const group of state.dup || []) {
    const keeper = pickKeeper(group);
    group.files.forEach((path, index) => {
      if (path === keeper || !state.dupChecked.has(path)) return;
      items.push({ path, hash: group.hash, size: group.size, mtime: group.mtimes[index] || 0 });
    });
  }
  if (!items.length) {
    toast("请先勾选要清理的重复文件");
    return;
  }
  const total = items.reduce((sum, item) => sum + item.size, 0);
  const lines = [`将把 ${items.length} 个副本移到回收站，约 ${fmtSize(total)}。`,
                 "每组保留一份；删除前会重新核对内容，扫描后变过的文件会跳过。"];
  if (!(await showConfirm("确认清理重复文件", lines, "移到回收站"))) return;
  button.disabled = true;
  button.textContent = "清理中…";
  try {
    const report = await invoke("delete_dups", { items });
    const result = [`移到回收站 ${report.deleted} 个，释放 ${fmtSize(report.freed)}。`];
    for (const reason of (report.reasons || []).slice(0, 5)) result.push(reason);
    await showResult("清理完成", result);
  } catch (error) {
    toast("清理失败：" + error);
  }
  button.disabled = false;
  button.textContent = "一键清理";
  state.dup = null;
  state.dupChecked = new Set();
  await scanDup();
}

/* ---------- 弹窗 ---------- */
function showConfirm(title, lines, confirmText) {
  return modal(title, lines, confirmText, "取消");
}

function showResult(title, lines) {
  return modal(title, lines, null, null);
}

function modal(title, lines, confirmText, cancelText) {
  return new Promise((resolve) => {
    const mask = el("div", "modal-mask");
    const box = el("div", "modal");
    box.appendChild(el("h3", null, title));
    const body = el("div", "modal-body");
    for (const line of lines) body.appendChild(el("p", null, line));
    box.appendChild(body);
    const actions = el("div", "modal-actions");
    if (confirmText && cancelText) {
      const cancel = el("button", "btn btn-ghost", cancelText);
      cancel.addEventListener("click", () => {
        mask.remove();
        resolve(false);
      });
      actions.appendChild(cancel);
    }
    const ok = el("button", "btn btn-primary", confirmText || "知道了");
    ok.addEventListener("click", () => {
      mask.remove();
      resolve(true);
    });
    actions.appendChild(ok);
    box.appendChild(actions);
    mask.appendChild(box);
    document.body.appendChild(mask);
    ok.focus();
  });
}

/* ---------- 启动 ---------- */
async function loadDisk() {
  try {
    state.disk = await invoke("disk_info");
  } catch (error) {
    state.disk = null;
  }
  if (state.page === "clean") renderClean();
}

function bindWindowButtons() {
  $("#btn-min")?.addEventListener("click", () => appWindow?.minimize());
  $("#btn-max")?.addEventListener("click", () => appWindow?.toggleMaximize());
  $("#btn-close")?.addEventListener("click", () => appWindow?.close());
  $("#history-link")?.addEventListener("click", (event) => {
    event.preventDefault();
    showHistory();
  });
  $("#rules-link")?.addEventListener("click", (event) => {
    event.preventDefault();
    updateRules();
  });
  $("#rules-rollback-link")?.addEventListener("click", (event) => {
    event.preventDefault();
    rollbackRules();
  });
}

async function loadDrives() {
  try {
    state.drives = await invoke("list_drives");
  } catch (error) {
    state.drives = [];
  }
  if (state.page === "frag") renderFrag();
}

function init() {
  bindWindowButtons();
  state.scanningClean = true;
  renderNav();
  showPage("clean");
  loadDisk();
  loadDrives();
  scanCategories();
}

document.addEventListener("DOMContentLoaded", init);
