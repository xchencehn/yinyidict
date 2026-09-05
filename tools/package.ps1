# 打发布包。先 cargo build --release --workspace。
#
#   pwsh tools/package.ps1 [-Version v0.1.0]
#
# 包里只放我们自己的东西（两个 exe + 说明 + 引导脚本），**不含任何词库数据**：
# 那些数据各有各的授权（尤其 ECDICT 授权不明确），不该由我们再分发。
# 用户跑一次 setup.bat，程序自己从各家官方地址取。
param([string]$Version = 'v0.1.0')
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$rel  = Join-Path $root 'target\release'
$out  = Join-Path $root "dist\yinyidict-$Version-windows-x64"

foreach ($f in @('dict.exe', 'dict-build.exe')) {
  if (-not (Test-Path (Join-Path $rel $f))) {
    throw "缺 $f —— 先跑 cargo build --release --workspace"
  }
}

if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory -Force -Path $out | Out-Null

Copy-Item (Join-Path $rel 'dict.exe')       $out
Copy-Item (Join-Path $rel 'dict-build.exe') $out
Copy-Item (Join-Path $PSScriptRoot 'dist\*') $out

$zip = "$out.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path "$out\*" -DestinationPath $zip -CompressionLevel Optimal

$mb = (Get-Item $zip).Length / 1MB
"打好了：$zip  ({0:N1} MB)" -f $mb
Get-ChildItem $out | ForEach-Object { "  $($_.Name)" }
