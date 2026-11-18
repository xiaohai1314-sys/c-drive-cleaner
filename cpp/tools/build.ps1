param(
    [ValidateSet("debug", "release")]
    [string]$Config = "debug",
    [switch]$Test,
    [switch]$Clean
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$preset = "x64-$Config"

$candidates = @(
    $env:CDC_CMAKE,
    "D:\tools\cmake-4.4.4-windows-x86_64\bin\cmake.exe"
)
$cmake = $null
foreach ($candidate in $candidates) {
    if ($candidate -and (Test-Path -LiteralPath $candidate)) {
        $cmake = $candidate
        break
    }
}
if (-not $cmake) {
    $found = Get-Command cmake -ErrorAction SilentlyContinue
    if ($found) { $cmake = $found.Source }
}
if (-not $cmake) {
    throw "找不到 CMake 4.4+。请下载 cmake-4.4.4-windows-x86_64 并设置环境变量 CDC_CMAKE。"
}
$cmakeDir = Split-Path -Parent $cmake
$ctest = Join-Path $cmakeDir "ctest.exe"

if ($Clean) {
    Remove-Item -Recurse -Force (Join-Path $root "build/$preset") -ErrorAction SilentlyContinue
}

Write-Host "使用 CMake: $cmake"
& $cmake --preset $preset
if ($LASTEXITCODE -ne 0) { throw "配置失败" }

& $cmake --build --preset $preset
if ($LASTEXITCODE -ne 0) { throw "编译失败" }

if ($Test) {
    & $cmake --build --preset $preset --target cdc_tests
    if ($LASTEXITCODE -ne 0) { throw "测试目标编译失败" }
    $configName = if ($Config -eq "debug") { "Debug" } else { "Release" }
    & $ctest --test-dir (Join-Path $root "build/$preset") -C $configName --output-on-failure
    if ($LASTEXITCODE -ne 0) { throw "测试失败" }
}

Write-Host "完成: $(Join-Path $root "build/$preset/$configName/c-drive-cleaner-cpp.exe")"
