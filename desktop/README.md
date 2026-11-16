# Tauri 桌面版（Rust 前端）

这是「C盘清理工具」的 Rust + Tauri 重写版本，和根目录的 Python 版并存、互不影响。
Python 版继续可用，Rust 版按页面逐步迁移。

## 目录

```
desktop/
  Cargo.toml           Rust 依赖
  build.rs             tauri-build 入口
  tauri.conf.json      窗口、打包、资源（rules.json / mftreader.exe）
  capabilities/        窗口权限
  src/main.rs          后端命令：磁盘信息、清理项统计、大文件、重复文件
ui/                    前端（纯 HTML/CSS/JS，无 Node 构建链）
  index.html
  style.css
  app.js
```

## 已迁移

- 窗口外壳：侧栏 8 项、页面切换、窗口最小化/最大化/关闭
- 首页「C盘瘦身」：C 盘占用条 + 按 `rules.json` 统计的清理项卡片（推荐/谨慎分组、勾选、合计），勾选后可直接清理
- 碎片清理：调用 `defrag.exe /A /V` 分析（中英文输出都能解析）、`/O` 优化，识别固态盘（走 TRIM），按碎片率画分布格
- 隐私清理：浏览器历史 / Cookie / 下载记录（Chromium 系 + Firefox，SQL 在副本上执行后写回）、最近敏感文件、最近文档、UserAssist、媒体播放记录；注册表删除前用 `reg.exe` 导出备份
- 大文件专清：MFT 枚举（复用 `mftreader.exe`），按类型页签筛选、勾选、排序，清理时移到回收站
- 重复文件专清：默认只扫 C 盘，跳过系统目录，采样哈希 + 全文件哈希（Rust 并行），按组显示保留项，删除前重新核对内容
- 微信 / QQ 专清：按目录语义分类聊天视频/图片/语音/接收文件/缓存/日志/朋友圈，按月分组，默认勾选缓存和日志，清理走回收站
- 文件粉碎：随机数据覆盖写入后删除，拒绝系统目录、受保护路径和链接；目录逐个文件粉碎后清空
- 清理历史：每次清理写 `logs/YYYYMMDD_HHMMSS.log`，侧栏「清理历史」查看
- 规则更新：从 `rules_update.json` 里的 https 地址拉取规则，校验后保存到 `%LOCALAPPDATA%\C盘清理工具\rules_remote.json`，下次扫描合并

## 删除安全

所有删除都走同一套检查（[src/safety.rs](src/safety.rs)），宁可跳过不误删：

- 永不清理名单：`\Recovery`、`System Volume Information`、`usmt.ppkg`
- 受保护路径：Windows 目录、Program Files、ProgramData、盘符根、关键系统文件（pagefile 等）
- 用户文档库：文档/桌面/下载/图片/音乐/视频，以及浏览器保存的密码库（Login Data、logins.json 等）
- 重解析点：文件本身和每一级父目录都检查，避免被换成链接
- 外置规则一律按「不可信」处理：目标必须落在临时目录或 `appdata_path_allowlist.json` 的目录模板内
- 重复文件删除前重新计算 BLAKE3 哈希，并核对大小和修改时间；变过的跳过
- 清理项遵守规则里的通配符、排除项和「只清 N 分钟前的文件」
- 删除按句柄进行（`FILE_FLAG_OPEN_REPARSE_POINT`），失败时去掉只读属性重试，仍失败则登记重启后删除
- 大文件和重复副本走回收站，失败不会退回到永久删除
- 隐私清理不碰浏览器密码库；注册表项删除前先导出 `.reg` 备份，导出失败就跳过
- 文件粉碎不可恢复，界面有二次确认；系统目录、受保护路径和链接一律拒绝

## 与 Python 版的已知差异

- USB 使用痕迹只统计不清理（改系统设备记录风险高）
- 隐私清理的「电脑搜索记录」没有迁移；其余浏览器/注册表/媒体痕迹已覆盖
- 规则更新保存到 `%LOCALAPPDATA%`，不改动程序内置的 `rules.json`

## 当前状态（2026-11-13 实测，调试构建）

| 项目 | 结果 |
|---|---|
| 窗口出现 | 约 3 秒（发布构建会更快） |
| 首页清理项统计 | 1.0 秒，30 条，合计 562 MB |
| 大文件扫描 | 0.7 秒，933 个（MFT 缓存命中） |
| 重复文件扫描 | 4.2 秒，282 组（首次约 20 秒） |
| 碎片分析 | 约 13–18 秒（defrag 分析本身耗时） |
| 微信/QQ 扫描 | 约 0.7 秒（微信 824 个文件） |
| 隐私扫描 | 约 0.5 秒（24 项，14 项有数据） |

后端可以用 `--selftest` 单独验证；设置 `CDC_TRACE=1` 会打印扫描各阶段耗时。

## 构建与运行

```bash
cd desktop
cargo build            # 调试版
cargo run              # 启动窗口
cargo test             # 路径安全和删除逻辑的单元测试
cargo build --release  # 发布版
./target/debug/c-drive-cleaner.exe --selftest   # 不开窗口，直接跑一遍后端扫描
```

首次构建会下载 Tauri 依赖，需要几分钟；之后增量编译很快。
程序启动时会检查管理员权限（读 `$MFT` 需要），不是管理员会用 UAC 重新拉起自己。

> 编译特别慢时，通常是杀毒软件在逐个扫描 `target` 目录。把 `desktop/target` 和
> `C:\Users\Administrator\.cargo` 加入杀毒软件的信任目录可以快很多。

## 和 Python 版的对应关系

| Python | Rust |
|---|---|
| `categories.get_categories()` + `scanner.scan_category` | `scan_categories`（解析 `rules.json`，展开环境变量与通配符，按规则遍历） |
| `fastscan.iter_files` + `scanner.scan_large_files` | `scan_big`（调用 `mftreader.exe`，多盘并行） |
| `dupes.find_duplicates` | `scan_dups`（rayon 并行采样/全文件哈希） |
| `main.py` 的 Tkinter 界面 | `ui/` + `src/main.rs` |

## 说明

- 规则文件 `rules.json` 在编译时嵌入二进制，改规则后需要重新编译。
- `mftreader.exe` 按「exe 同目录 → 项目根目录」顺序查找；打包时作为资源带出。
- 删除/回收站/注册表等写操作尚未迁移，界面上对应按钮只提示「还没迁移」。
