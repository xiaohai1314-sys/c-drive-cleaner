# C 盘清理工具

Windows 下的图形化 C 盘清理工具，先扫描、再勾选、后清理。
Rust + Tauri 2 实现：后端负责扫描、删除、粉碎和隐私清理，界面是 Vue 3 + Vite + TypeScript（`web/`）。

## 功能

| 页面 | 说明 |
|---|---|
| C盘瘦身 | 按 `rules.json` 统计各清理项占用，勾选后清理 |
| 碎片清理 | 调用系统 `defrag` 分析和优化，固态盘走 TRIM |
| 隐私清理 | 浏览器历史 / Cookie / 下载记录、最近文件、注册表痕迹 |
| 大文件专清 | 解析 NTFS `$MFT` 快速枚举，大文件移到回收站 |
| 重复文件专清 | 采样哈希 + 全文件哈希，每组保留一份，删除前重新核对 |
| 微信/QQ 专清 | 按目录语义分类聊天媒体、缓存和日志 |
| 文件粉碎 | 系统安全随机数覆盖后删除；固态盘上不保证物理层面不可恢复 |
| 清理历史 | 查看每次清理的日志 |

侧栏底部还有「清理历史」和「规则更新」。

## 构建与运行

需要 Rust 1.77+、Node 20+ 和 WebView2 运行时（Win10 1803+ 一般自带）。

前端在 `web/`（Vue 3 + Vite + TypeScript），构建产物 `web/dist` 在编译期嵌入 exe，
所以改了界面必须先构建前端，直接 `cargo build` 会把旧的 `web/dist` 打进去：

```bash
npm --prefix web install   # 首次
pwsh scripts/build.ps1     # 先 npm run build，再 cargo build --release
```

分开跑也一样：

```bash
npm --prefix web run build
cargo build --release --manifest-path desktop/Cargo.toml
```

调试版、测试和自检：

```bash
cargo build --manifest-path desktop/Cargo.toml            # 调试版
cargo run --manifest-path desktop/Cargo.toml              # 启动窗口
cargo test --manifest-path desktop/Cargo.toml             # 单元测试
./desktop/target/debug/c-drive-cleaner.exe --selftest     # 不开窗口跑一遍后端扫描
```

程序启动时会检查管理员权限（读 `$MFT` 需要），不是管理员会用 UAC 重新拉起自己。

> 编译特别慢时，通常是杀毒软件在逐个扫描 `desktop/target`。把它和
> `C:\Users\<用户>\.cargo` 加入杀毒软件的信任目录会快很多。

## 数据文件

- `rules.json`：清理规则，编译时嵌入二进制，改规则后需要重新编译
- `appdata_path_allowlist.json`：外置/远程规则允许的 AppData 目录模板
- `rules_update.json`：规则更新地址（只接受 https），更新结果写到
  `%LOCALAPPDATA%\C盘清理工具\rules_remote.json`
- 清理日志：`%LOCALAPPDATA%\C盘清理工具\logs`，保留最近 100 份

## 安全设计

- 永不清理名单（Recovery、System Volume Information 等）、受保护路径、用户文档库、浏览器密码库
- 重解析点（符号链接/目录联接）不跟随，删除按句柄进行
- 大文件和重复副本走回收站；重复文件删除前重新核对哈希、大小和修改时间
- 隐私清理的注册表项删除前用 `reg.exe` 导出 `.reg` 备份，导出失败就跳过
- 文件粉碎有二次确认，系统目录和受保护路径直接拒绝；固态盘有磨损均衡，覆盖只能尽力清除
- 系统瘦身的更新缓存、转储目录逐文件按句柄删除，不跟随重解析点；清理更新缓存前先暂停 wuauserv/BITS，传递优化缓存交给系统 `Delete-DeliveryOptimizationCache`
- 微信/QQ 搬家逐文件核对后才删除原目录；中断恢复记录只接受微信/QQ 数据目录及其同级备份

更多实现细节见 [desktop/README.md](desktop/README.md)。
