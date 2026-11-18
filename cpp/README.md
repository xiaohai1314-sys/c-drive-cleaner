# C 盘清理工具(C++ 版)

这是 Rust + Tauri 版的 C++ 重写:后端用 C++20 + Win32/NT API,界面继续使用仓库根
`web/` 的 Vue 3 前端,通过 WebView2 承载。C++ 代码独立放在 `cpp/`,Rust 版保持可用。

## 目录

```
cpp/
  CMakeLists.txt / CMakePresets.json
  cmake/                # 依赖检查、前端构建、shim 嵌入
  tools/fetch_deps.ps1  # 下载并校验依赖(CMake 不联网)
  tools/build.ps1       # 一键配置 + 编译(+ 测试)
  tools/versions.json   # 固定版本与 SHA256
  third_party/          # fetch_deps.ps1 填充,已 gitignore
  src/app/              # 无边框窗口、WebView2 宿主、IPC、文件对话框、shim.js
  src/core/             # 与 Rust 模块对应的移植
  src/core/commands/    # 37 个命令的协议层
  src/util/             # UTF-8/16、路径、环境变量、Win32 辅助
  tests/                # 单元测试
```

## 构建

前置条件:

- Windows 10/11 x64
- Visual Studio 2022 Build Tools(VC++ x64 工作负载 + Windows SDK)
- Node.js 20+(前端构建)
- CMake 4.4+。**注意**:VS 自带的 CMake 3.31 在中文路径下会崩溃,请使用官方
  `cmake-4.4.4-windows-x86_64`(解压到任意 ASCII 路径),并设置 `CDC_CMAKE`,
  或放到 `D:\tools\cmake-4.4.4-windows-x86_64\bin\cmake.exe`。

```powershell
# 1) 拉依赖(首次,固定版本 + SHA256)
powershell -ExecutionPolicy Bypass -File cpp/tools/fetch_deps.ps1

# 2) 构建(脚本会自动找 CMake)
powershell -ExecutionPolicy Bypass -File cpp/tools/build.ps1 -Config debug -Test

# 3) 运行
cpp\build\x64-debug\Debug\c-drive-cleaner-cpp.exe

# 4) 不开窗口跑一遍后端
cpp\build\x64-debug\Debug\c-drive-cleaner-cpp.exe --selftest
```

手动构建:

```bash
cmake --preset x64-debug
cmake --build --preset x64-debug
ctest --preset x64-debug --output-on-failure
```

前端产物 `web/dist` 由 CMake 在源码变化时自动执行 `npm run build`,并复制到 exe 旁边;
`rules.json`、`appdata_path_allowlist.json`、`rules_update.json` 也会复制过去。

## 当前进度

- [x] WebView2 外壳、无边框窗口、DPI、最小化/最大化/关闭、拖拽区
- [x] `window.__TAURI__` 兼容层(IPC、窗口控制、文件对话框、`imthumb` 占位)
- [x] `disk_info`、`user_profile`、`list_drives`、`task_status`、`cancel_task`、`scan_status`、`cancel_scan`
- [x] `history_list`、`history_read`、`reveal_path`、`open_path`
- [x] 任务注册表、日志、时间、UTF/路径/环境变量工具
- [ ] `safety.rs` 句柄相对路径安全与删除
- [ ] `rules.json` 解析、`scan_categories`、`clean_categories`
- [ ] `$MFT`、大文件、重复文件、隐私、微信/QQ、系统瘦身

未移植的命令目前返回空数据或明确报错("尚未移植"),不会静默执行危险操作。
