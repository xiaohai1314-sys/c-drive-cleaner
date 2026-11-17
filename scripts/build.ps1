# 标准构建入口：先构建前端，再编译 Tauri 可执行文件。
# 直接跑 cargo build 不会执行前端构建，会把旧的 web/dist 打进 exe。
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    npm --prefix web run build
    if ($LASTEXITCODE -ne 0) { throw "前端构建失败" }
    cargo build --release --manifest-path desktop/Cargo.toml
    if ($LASTEXITCODE -ne 0) { throw "Rust 构建失败" }
} finally {
    Pop-Location
}
