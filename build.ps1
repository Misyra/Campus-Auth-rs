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
#   ├── docker-compose.yml           # 默认 GHCR 镜像编排
#   ├── docker-compose.build.yml     # 本地源码构建覆盖
#   ├── .dockerignore                # Docker 上下文排除
#   ├── docker/                      # Docker 辅助文件（entrypoint.sh 等）
#   ├── README.md                    # 快速开始
#   ├── resources/                   # 托盘图标 / task-recorder 等静态资源
#   ├── src/ + frontend/             # Docker 镜像所需的最小源码构建上下文
#   ├── python_worker/               # Python Worker 源码（运行时按需引导 uv 环境）
#   └── docs/                        # 更新/更改/已知问题 + 离线指南
# 打包产物仅在 $Out（默认 dist/，见 .gitignore /dist/）；不再向项目根目录复制 exe，
# 版本以 `target/release/campus-auth --version`（= Cargo.toml）为准，避免根残留误导。
# 要求 pwsh 7+（UTF-8），Windows PowerShell 5.1 会按 ANSI 解析中文导致乱码。

param(
    [switch]$SkipFrontend,
    [string]$OutDir = "dist"
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path

# 所有递归删除和目录替换都先验证绝对路径；分发标记不能绕过源码或数据保护。
function Assert-PackageOutput {
    param([string]$Path, [string]$RepositoryRoot)
    $full = [System.IO.Path]::GetFullPath($Path)
    $repo = [System.IO.Path]::GetFullPath($RepositoryRoot)
    $relative = [System.IO.Path]::GetRelativePath($full, $repo)
    if ($full -eq [System.IO.Path]::GetPathRoot($full) -or
        $relative -eq "." -or (-not $relative.StartsWith("..") -and -not [System.IO.Path]::IsPathRooted($relative))) {
        throw "输出目录不能是磁盘根目录、仓库或仓库的祖先：$full"
    }
    $within = [System.IO.Path]::GetRelativePath($repo, $full)
    if (-not $within.StartsWith("..") -and -not [System.IO.Path]::IsPathRooted($within)) {
        $first = ($within -split '[\\/]')[0]
        if ($first -in @("src", "frontend", "python_worker", "resources", "docs", "tests", "docker", "target", ".git", ".github", "config", "tasks", "logs", "environment", "update", ".campus_network_auth")) {
            throw "输出目录不能位于源码、构建缓存或用户数据目录内：$full"
        }
    }
    $ancestor = $full
    while ($ancestor) {
        if (Test-Path -LiteralPath $ancestor) {
            $item = Get-Item -LiteralPath $ancestor -Force
            if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
                throw "输出路径不能经过符号链接或联接：$ancestor"
            }
        }
        $parent = [System.IO.Path]::GetDirectoryName($ancestor)
        if ($parent -eq $ancestor) { break }
        $ancestor = $parent
    }
    if (Test-Path -LiteralPath $full) {
        if (-not (Test-Path -LiteralPath $full -PathType Container)) { throw "输出路径不是目录：$full" }
        foreach ($protected in @(".git", "config", "tasks", "logs", "environment", "update", ".campus_network_auth")) {
            if (Test-Path -LiteralPath (Join-Path $full $protected)) { throw "输出目录包含源码或用户数据：$protected" }
        }
        $children = @(Get-ChildItem -LiteralPath $full -Force)
        $marker = Join-Path $full "campus-auth.portable"
        if ($children.Count -gt 0 -and
            (-not (Test-Path -LiteralPath $marker -PathType Leaf) -or
            (Get-Content -LiteralPath $marker -Raw).Trim() -ne "campus-auth-portable-v1")) {
            throw "拒绝覆盖未标记的非空目录：$full；请选择专用空目录"
        }
    }
    return $full
}

function Assert-NativeSuccess {
    param([int]$ExitCode, [string]$Step)
    if ($ExitCode -ne 0) { throw "$Step 失败（退出码 $ExitCode），停止打包" }
}

# 缓存删除必须限定在本轮组装目录，并拒绝经过联接的路径。
function Assert-PackageChild {
    param([string]$Path, [string]$OutputRoot)
    $full = [System.IO.Path]::GetFullPath($Path)
    $output = [System.IO.Path]::GetFullPath($OutputRoot)
    $relative = [System.IO.Path]::GetRelativePath($output, $full)
    if ($relative -eq "." -or $relative -eq ".." -or $relative.StartsWith(".." + [System.IO.Path]::DirectorySeparatorChar) -or [System.IO.Path]::IsPathRooted($relative)) {
        throw "缓存路径越过组装目录：$full"
    }
    $ancestor = $full
    while ($ancestor -and $ancestor -ne $output) {
        if ((Get-Item -LiteralPath $ancestor -Force).Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
            throw "缓存路径不能经过符号链接或联接：$ancestor"
        }
        $ancestor = [System.IO.Path]::GetDirectoryName($ancestor)
    }
    return $full
}

$requestedOut = if ([System.IO.Path]::IsPathRooted($OutDir)) { $OutDir } else { Join-Path $Root $OutDir }
$finalOut = Assert-PackageOutput -Path $requestedOut -RepositoryRoot $Root

# ---- 1/4 前端构建（产物供 rust-embed 嵌入） ----
if (-not $SkipFrontend) {
    Write-Host "=== 1/4 前端构建 ==="
    Push-Location (Join-Path $Root "frontend")
    try {
        npm ci
        Assert-NativeSuccess -ExitCode $LASTEXITCODE -Step "安装前端依赖"
        npm run build
        Assert-NativeSuccess -ExitCode $LASTEXITCODE -Step "构建前端"
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
    cargo build --release --locked
    Assert-NativeSuccess -ExitCode $LASTEXITCODE -Step "构建 Rust"
} finally {
    Pop-Location
}

if ($IsWindows) {
    $cargoVersion = [regex]::Match((Get-Content -LiteralPath (Join-Path $Root "Cargo.toml") -Raw), '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
    foreach ($binary in @("campus-auth.exe", "campus-auth-helper.exe")) {
        $built = Get-Item -LiteralPath (Join-Path $Root "target/release/$binary")
        if ($built.VersionInfo.ProductVersion -ne $cargoVersion) {
            throw "$binary 的版本资源与 Cargo 版本不一致，拒绝打包"
        }
    }
}

# ---- 3/4 组装便携目录 ----
Write-Host "=== 3/4 组装便携目录 ==="
# 本轮成功产物在同级新目录组装完成后才替换旧包，失败时保留旧产物。
$Out = Assert-PackageOutput -Path "$finalOut.building-$([guid]::NewGuid().ToString('N'))" -RepositoryRoot $Root
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
# 明确区分携带 Cargo 源码的便携包与开发工程，卸载时仍保留源码仓库守卫。
Set-Content -LiteralPath (Join-Path $Out "campus-auth.portable") -Value "campus-auth-portable-v1" -Encoding utf8NoBOM

Copy-Item (Join-Path $Root "resources") (Join-Path $Out "resources") -Recurse

# Dockerfile 是源码构建镜像；便携包需同时带最小构建上下文，不能只放一个无法执行的入口文件。
Copy-Item (Join-Path $Root "src") (Join-Path $Out "src") -Recurse
$frontendDst = Join-Path $Out "frontend"
New-Item -ItemType Directory -Path $frontendDst | Out-Null
Get-ChildItem (Join-Path $Root "frontend") -Force |
    Where-Object { $_.Name -notin @("node_modules", "dist", ".vite", "coverage") } |
    ForEach-Object { Copy-Item $_.FullName (Join-Path $frontendDst $_.Name) -Recurse -Force }
foreach ($f in @("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "build.rs", "openapi.json")) {
    Copy-Item (Join-Path $Root $f) (Join-Path $Out $f) -Force
}

# 日志与指南随包：README 中的文档链接在便携包内仍可用。
$docsDst = Join-Path $Out "docs/guides"
New-Item -ItemType Directory -Path $docsDst -Force | Out-Null
Copy-Item (Join-Path $Root "docs/guides/*.md") $docsDst -Force
foreach ($doc in @("updatelog.md", "changelog.md", "known-issues.md")) {
    Copy-Item (Join-Path $Root "docs/$doc") (Join-Path $Out "docs/$doc") -Force
}

# 复制 python_worker 时排除本地虚拟环境（运行时按需重建），避免先全量复制再删除的双重 IO；
# __pycache__ 与 release.yml 口径对齐一并排除（运行时自动再生）
$workerDst = Join-Path $Out "python_worker"
New-Item -ItemType Directory -Path $workerDst | Out-Null
Get-ChildItem (Join-Path $Root "python_worker") -Force | Where-Object { $_.Name -ne ".venv" } | ForEach-Object {
    Copy-Item $_.FullName (Join-Path $workerDst $_.Name) -Recurse -Force
}
Get-ChildItem $workerDst -Recurse -Directory -Filter "__pycache__" | ForEach-Object {
    $cache = Assert-PackageChild -Path $_.FullName -OutputRoot $Out
    Remove-Item -LiteralPath $cache -Recurse -Force
}
# 清理开发/运行时缓存（与 .dockerignore / .gitignore 口径对齐，避免污染便携包）
foreach ($d in @(".pytest_cache", ".tmp-uv-cache", ".mypy_cache", ".ruff_cache", "captures", "build", "dist", "node_modules")) {
    Get-ChildItem $workerDst -Recurse -Directory -Filter $d -ErrorAction SilentlyContinue | ForEach-Object {
        $cache = Assert-PackageChild -Path $_.FullName -OutputRoot $Out
        Remove-Item -LiteralPath $cache -Recurse -Force -ErrorAction SilentlyContinue
    }
    $top = Join-Path $workerDst $d
    if (Test-Path -LiteralPath $top) {
        $cache = Assert-PackageChild -Path $top -OutputRoot $Out
        Remove-Item -LiteralPath $cache -Recurse -Force -ErrorAction SilentlyContinue
    }
}
Get-ChildItem $workerDst -Recurse -File -Filter "*.pyc" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

# Docker 部署文件（与 release.yml 口径一致）
# LICENSE 随包：AGPL §6 二进制分发需附带许可证文本
foreach ($f in @("LICENSE", "Dockerfile", "docker-compose.yml", "docker-compose.build.yml", ".dockerignore", "README.md")) {
    $src = Join-Path $Root $f
    if (Test-Path $src) { Copy-Item $src $Out -Force }
}
$dockerSrc = Join-Path $Root "docker"
if (Test-Path $dockerSrc) {
    Copy-Item $dockerSrc (Join-Path $Out "docker") -Recurse -Force
}

# 构建后再次验证，避免目标路径在构建期间被替换为联接。
$null = Assert-PackageOutput -Path $finalOut -RepositoryRoot $Root
$null = Assert-PackageOutput -Path $Out -RepositoryRoot $Root
$previousOut = Assert-PackageOutput -Path "$finalOut.previous-$([guid]::NewGuid().ToString('N'))" -RepositoryRoot $Root
$hadPrevious = Test-Path -LiteralPath $finalOut
if ($hadPrevious) { Move-Item -LiteralPath $finalOut -Destination $previousOut }
try {
    Move-Item -LiteralPath $Out -Destination $finalOut
} catch {
    if ($hadPrevious) { Move-Item -LiteralPath $previousOut -Destination $finalOut }
    throw
}
if ($hadPrevious) {
    $null = Assert-PackageOutput -Path $previousOut -RepositoryRoot $Root
    Remove-Item -LiteralPath $previousOut -Recurse -Force
}
$Out = $finalOut

# ---- 4/4 完成 ----
$sizeMB = [math]::Round(
    ((Get-ChildItem $Out -Recurse -File | Measure-Object Length -Sum).Sum / 1MB),
    1
)

Write-Host "=== 4/4 完成 ==="
Write-Host "便携版输出: $Out（约 ${sizeMB} MB，解压后直接运行 campus-auth$exeSuffix）"
Write-Host "本地冒烟: $Out\campus-auth$exeSuffix --status（勿用 target/debug 测 release 行为）"
Write-Host "Docker 部署: 解压后 docker compose pull && docker compose up -d（默认 GHCR 预构建镜像）"
