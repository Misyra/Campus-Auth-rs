# 便携版打包脚本：前端构建 → Rust release 构建 → 组装便携目录
#
# 用法：
#   pwsh ./build.ps1                 # 完整流程（前端 + Rust + 组装）
#   pwsh ./build.ps1 -SkipFrontend   # 跳过前端构建（使用现有 frontend/dist）
#   pwsh ./build.ps1 -OutDir dist-test  # 自定义输出目录
#
# 输出目录结构（解压即用，默认输出到项目根目录 dist/）：
#   dist/
#   ├── campus-auth(.exe)            # 主程序（前端已嵌入）
#   ├── campus-auth-helper(.exe)     # 更新替换助手
#   ├── LICENSE                     # AGPL-3.0-only 全文（二进制分发必备）
#   ├── Dockerfile                   # Docker 构建文件
#   ├── docker-compose.yml           # Docker 编排
#   ├── .dockerignore                # Docker 上下文排除
#   ├── docker/                      # Docker 辅助文件（entrypoint.sh 等）
#   ├── README.md                    # 快速开始
#   ├── resources/                   # 托盘图标 / task-recorder 等静态资源
#   ├── python_worker/               # Python Worker 源码（运行时按需引导 uv 环境）
#   └── docs/guides/                 # 离线指南（docs/guides/*.md）
# 打包产物仅在 $Out（默认 dist/，见 .gitignore /dist/）；不再向项目根目录复制 exe，
# 版本以 `target/release/campus-auth --version`（= Cargo.toml）为准，避免根残留误导。
# 要求 pwsh 7+（UTF-8），Windows PowerShell 5.1 会按 ANSI 解析中文导致乱码。

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

# 二进制（Windows 带 .exe）
$exeSuffix = ""
if ($IsWindows -or $env:OS -match "Windows") { $exeSuffix = ".exe" }
$releaseDir = Join-Path $Root "target\release"
# 兼容直接执行与 CI 交叉编译：优先取 target/release，缺失则报错提示
$mainExe = Join-Path $releaseDir "campus-auth$exeSuffix"
$helperExe = Join-Path $releaseDir "campus-auth-helper$exeSuffix"
if (-not (Test-Path $mainExe)) { throw "未找到 $mainExe，请先 cargo build --release" }
if (-not (Test-Path $helperExe)) { throw "未找到 $helperExe，请先 cargo build --release" }
Copy-Item $mainExe $Out
Copy-Item $helperExe $Out

Copy-Item (Join-Path $Root "resources") (Join-Path $Out "resources") -Recurse

# 指南随包：全部 docs/guides/*.md 供离线查阅（含 user-guide/task-writing-guide 等）
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
# 清理开发/运行时缓存（与 .dockerignore / .gitignore 口径对齐，避免污染便携包）
foreach ($d in @(".pytest_cache", ".tmp-uv-cache", ".mypy_cache", ".ruff_cache", "captures", "build", "dist", "node_modules")) {
    Get-ChildItem $workerDst -Recurse -Directory -Filter $d -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    $top = Join-Path $workerDst $d
    if (Test-Path $top) { Remove-Item $top -Recurse -Force -ErrorAction SilentlyContinue }
}
Get-ChildItem $workerDst -Recurse -File -Filter "*.pyc" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

# Docker 部署文件（与 release.yml 口径一致）
# LICENSE 随包：AGPL §6 二进制分发需附带许可证文本
foreach ($f in @("LICENSE", "Dockerfile", "docker-compose.yml", ".dockerignore", "README.md")) {
    $src = Join-Path $Root $f
    if (Test-Path $src) { Copy-Item $src $Out -Force }
}
$dockerSrc = Join-Path $Root "docker"
if (Test-Path $dockerSrc) {
    Copy-Item $dockerSrc (Join-Path $Out "docker") -Recurse -Force
}

# ---- 4/4 完成 ----
$sizeMB = [math]::Round(
    ((Get-ChildItem $Out -Recurse -File | Measure-Object Length -Sum).Sum / 1MB),
    1
)

Write-Host "=== 4/4 完成 ==="
Write-Host "便携版输出: $Out（约 ${sizeMB} MB，解压后直接运行 campus-auth$exeSuffix）"
Write-Host "本地冒烟: $Out\campus-auth$exeSuffix --status（勿用 target/debug 测 release 行为）"
Write-Host "Docker 部署: 解压后 docker compose up -d --build（需 Dockerfile/docker-compose.yml/docker/）"
