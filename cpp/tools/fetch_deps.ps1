# 下载并校验 C++ 版依赖，输出到 cpp/third_party/。可重复运行，已存在且哈希正确时跳过。
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.IO.Compression.FileSystem

$root = Split-Path -Parent $PSScriptRoot
$thirdParty = Join-Path $root "third_party"
$downloads = Join-Path $thirdParty "downloads"
New-Item -ItemType Directory -Force -Path $thirdParty, $downloads | Out-Null

$manifest = Get-Content (Join-Path $PSScriptRoot "versions.json") -Raw | ConvertFrom-Json

function Get-CheckedFile([string]$url, [string]$dest, [string]$sha) {
    if (Test-Path -LiteralPath $dest) {
        $actual = (Get-FileHash -LiteralPath $dest -Algorithm SHA256).Hash.ToLower()
        if ($actual -eq $sha.ToLower()) { return }
    }
    Write-Host "下载 $url"
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $dest
    $actual = (Get-FileHash -LiteralPath $dest -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $sha.ToLower()) {
        throw "SHA256 不匹配: $dest`n期望 $sha`n实际 $actual"
    }
}

function Extract-Entries([string]$zipPath, [string]$dest, [string[]]$prefixes, [string]$stripPrefix) {
    $zip = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
    try {
        foreach ($entry in $zip.Entries) {
            if ($entry.FullName.EndsWith("/")) { continue }
            $matched = $false
            foreach ($prefix in $prefixes) {
                if ($entry.FullName.StartsWith($prefix)) { $matched = $true; break }
            }
            if (-not $matched) { continue }
            $rel = $entry.FullName
            if ($stripPrefix -and $rel.StartsWith($stripPrefix)) { $rel = $rel.Substring($stripPrefix.Length) }
            $out = Join-Path $dest $rel
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $out) | Out-Null
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $out, $true)
        }
    } finally {
        $zip.Dispose()
    }
}

# WebView2
$wv = $manifest.webview2
$wvPkg = Join-Path $downloads "webview2-$($wv.version).nupkg"
Get-CheckedFile $wv.url $wvPkg $wv.sha256
$wvDir = Join-Path $thirdParty "webview2"
if (-not (Test-Path (Join-Path $wvDir "x64/WebView2LoaderStatic.lib"))) {
    Extract-Entries $wvPkg $wvDir @("build/native/include/", "build/native/x64/") "build/native/"
}

# nlohmann/json
$nl = $manifest.nlohmann
$nlFile = Join-Path $downloads "json.hpp"
Get-CheckedFile $nl.url $nlFile $nl.sha256
New-Item -ItemType Directory -Force -Path (Join-Path $thirdParty "nlohmann") | Out-Null
Copy-Item -LiteralPath $nlFile -Destination (Join-Path $thirdParty "nlohmann/json.hpp") -Force

# SQLite
$sq = $manifest.sqlite
$sqZip = Join-Path $downloads "sqlite-$($sq.version).zip"
Get-CheckedFile $sq.url $sqZip $sq.sha256
$sqDir = Join-Path $thirdParty "sqlite"
if (-not (Test-Path (Join-Path $sqDir "sqlite3.c"))) {
    Extract-Entries $sqZip $sqDir @($sq.archiveRoot) $sq.archiveRoot
}

# BLAKE3
$b3 = $manifest.blake3
$b3Zip = Join-Path $downloads "blake3-$($b3.version).zip"
Get-CheckedFile $b3.url $b3Zip $b3.sha256
$b3Dir = Join-Path $thirdParty "blake3"
if (-not (Test-Path (Join-Path $b3Dir "blake3.c"))) {
    Extract-Entries $b3Zip $b3Dir @($b3.archiveRoot) $b3.archiveRoot
}

Write-Host "依赖已就绪: $thirdParty"
