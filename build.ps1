# 便携版打包脚本：前端构建 → Rust release 构建 → 组装便携目录
#
# 用法：
#   .\build.ps1                 # 完整流程（前端 + Rust + 组装）
#   .\build.ps1 -SkipFrontend   # 跳过前端构建（使用现有 frontend/dist）
#   .\build.ps1 -OutDir dist-test  # 自定义输出目录
#
# 输出目录结构（解压即用，默认输出到项目根目录 dist/）：
#   dist/
#   ├── campus-auth.exe          # 主程序（前端已嵌入）
#   ├── campus-auth-helper.exe   # 更新替换助手
#   ├── python_worker/           # Python Worker 源码（运行时按需引导 uv 环境）
#   └── resources/               # 托盘图标 / task-recorder 等静态资源
#
# 打包完成后，额外将 campus-auth.exe 复制一份到项目根目录，方便直接运行测试。

param(
    [switch]$SkipFrontend,
    [string]$OutDir = "dist"
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path

# ---- 1/4 前端构建（产物供 rust-embed 嵌入） ----
if (-not $SkipFrontend) {
    Write-Host "=== 1/4 前端构建 ==="
    Push-Location (Join-Path $Root "frontend")
    try {
        npm ci
        npm run build
    } finally {
        Pop-Location
    }
} else {
    Write-Host "=== 1/4 跳过前端构建（使用现有 frontend/dist） ==="
    if (-not (Test-Path (Join-Path $Root "frontend\dist\index.html"))) {
        throw "frontend/dist 不存在，请先执行 npm run build 或去掉 -SkipFrontend"
    }
}

# ---- 2/4 Rust release 构建（两个 binary） ----
Write-Host "=== 2/4 Rust release 构建 ==="
Push-Location $Root
try {
    cargo build --release
} finally {
    Pop-Location
}

# ---- 3/4 组装便携目录 ----
Write-Host "=== 3/4 组装便携目录 ==="
$Out = Join-Path $Root $OutDir
if (Test-Path $Out) {
    Remove-Item $Out -Recurse -Force
}
New-Item -ItemType Directory -Path $Out | Out-Null

Copy-Item (Join-Path $Root "target\release\campus-auth.exe") $Out
Copy-Item (Join-Path $Root "target\release\campus-auth-helper.exe") $Out
Copy-Item (Join-Path $Root "resources") (Join-Path $Out "resources") -Recurse
# 指南随包：task-writing-guide 已 embed 兜底，此处附带全部 docs/guides/*.md 供离线查阅（不含 archive）
$docsDst = Join-Path $Out "docs/guides"
New-Item -ItemType Directory -Path $docsDst -Force | Out-Null
Copy-Item (Join-Path $Root "docs/guides/*.md") $docsDst -Force
# 复制 python_worker 时排除本地虚拟环境（运行时按需重建），避免先全量复制再删除的双重 IO；
# __pycache__ 与 release.yml 口径对齐一并排除（运行时自动再生）
$workerDst = Join-Path $Out "python_worker"
New-Item -ItemType Directory -Path $workerDst | Out-Null
Get-ChildItem (Join-Path $Root "python_worker") -Force | Where-Object { $_.Name -ne ".venv" } | ForEach-Object {
    Copy-Item $_.FullName (Join-Path $workerDst $_.Name) -Recurse -Force
}
Get-ChildItem $workerDst -Recurse -Directory -Filter "__pycache__" | Remove-Item -Recurse -Force

# ---- 4/4 完成 ----
$sizeMB = [math]::Round(
    ((Get-ChildItem $Out -Recurse -File | Measure-Object Length -Sum).Sum / 1MB),
    1
)

# 复制主程序到项目根目录，方便直接运行测试
$RootExe = Join-Path $Root "campus-auth.exe"
Copy-Item (Join-Path $Out "campus-auth.exe") $RootExe -Force

Write-Host "=== 4/4 完成 ==="
Write-Host "便携版输出: $Out（约 ${sizeMB} MB，解压后直接运行 campus-auth.exe）"
Write-Host "主程序已复制到项目根目录: $RootExe（直接运行即可测试）"
