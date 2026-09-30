# 只加载打包脚本的守卫函数，在隔离目录验证路径与失败停止，不执行构建或删除。
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $root "build.ps1"), [ref]$tokens, [ref]$parseErrors
)
if ($parseErrors.Count) { throw "打包脚本语法错误：$parseErrors" }
$functions = $ast.FindAll({ param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $node.Name -in @("Assert-PackageOutput", "Assert-NativeSuccess", "Assert-PackageChild")
}, $true)
foreach ($function in $functions) {
    . ([scriptblock]::Create($function.Extent.Text))
}
function Expect-Rejected {
    param([scriptblock]$Action, [string]$Name)
    $rejected = $false
    try { $null = & $Action } catch { $rejected = $true }
    if (-not $rejected) { throw "未拒绝危险操作：$Name" }
}
$fixture = Join-Path ([System.IO.Path]::GetTempPath()) "campus-package-safety-$([guid]::NewGuid().ToString('N'))"
$repository = Join-Path $fixture "repository"
New-Item -ItemType Directory -Path $repository -Force | Out-Null
foreach ($name in @("src", "config")) {
    New-Item -ItemType Directory -Path (Join-Path $repository $name) | Out-Null
}
Expect-Rejected { Assert-PackageOutput $repository $repository } "仓库根目录"
Expect-Rejected { Assert-PackageOutput $fixture $repository } "仓库祖先"
Expect-Rejected { Assert-PackageOutput ([System.IO.Path]::GetPathRoot($repository)) $repository } "磁盘根目录"
Expect-Rejected { Assert-PackageOutput (Join-Path $repository "src/nested") $repository } "源码子目录"
Expect-Rejected { Assert-PackageOutput (Join-Path $repository "config") $repository } "配置目录"
$unknown = Join-Path $fixture "unknown"
New-Item -ItemType Directory -Path $unknown | Out-Null
Set-Content -LiteralPath (Join-Path $unknown "keep.txt") -Value "保留"
Expect-Rejected { Assert-PackageOutput $unknown $repository } "未标记非空目录"
$link = Join-Path $fixture "linked-output"
$linkType = if ($IsWindows) { "Junction" } else { "SymbolicLink" }
New-Item -ItemType $linkType -Path $link -Target (Join-Path $repository "src") | Out-Null
Expect-Rejected { Assert-PackageOutput (Join-Path $link "nested") $repository } "联接目录逃逸"
$portable = Join-Path $fixture "portable"
New-Item -ItemType Directory -Path $portable | Out-Null
Set-Content -LiteralPath (Join-Path $portable "campus-auth.portable") -Value "campus-auth-portable-v1"
$null = Assert-PackageOutput $portable $repository
New-Item -ItemType Directory -Path (Join-Path $portable "tasks") | Out-Null
Expect-Rejected { Assert-PackageOutput $portable $repository } "带用户任务的便携目录"
$null = Assert-PackageOutput (Join-Path $fixture "new-output") $repository
$null = Assert-PackageChild (Join-Path $repository "src") $repository
Expect-Rejected { Assert-PackageChild $fixture $repository } "缓存删除越界"
Expect-Rejected { Assert-PackageChild $repository $repository } "缓存删除根目录"
Assert-NativeSuccess -ExitCode 0 -Step "成功模拟"
Expect-Rejected { Assert-NativeSuccess -ExitCode 7 -Step "失败模拟" } "原生命令失败"
# 运行已知会失败的系统命令，证明非零退出后不会进入下一阶段。
& $PSHOME/pwsh -NoProfile -Command "exit 7"
$failedExit = $LASTEXITCODE
Expect-Rejected { Assert-NativeSuccess -ExitCode $failedExit -Step "真实失败命令" } "真实退出码"
Write-Host "打包保护检查通过；隔离样本：$fixture"
