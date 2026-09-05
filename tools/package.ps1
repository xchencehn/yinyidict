# 打发布包。先 cargo build --release --workspace。
#
#   pwsh tools/package.ps1 [-Version v0.1.0]         精简包，约 4 MB
#   pwsh tools/package.ps1 [-Version v0.1.0] -Full   完整包，约 800 MB
#
# 精简包只放我们自己的东西（两个 exe + 说明 + 引导脚本），不含词库；
# 用户跑一次 setup.bat，程序自己从各家官方地址取。
#
# 完整包连词库、语音模型、推理运行库一起打进去，解压即用。
# **这是在再分发第三方数据**，所以包里必须带 THIRD-PARTY.txt：
# CC-CEDICT 是 CC BY-SA 3.0，署名和 ShareAlike 都是硬要求，不是礼貌。
param(
  [string]$Version = 'v0.1.0',
  [switch]$Full
)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$rel = Join-Path $root 'target\release'
$kind = if ($Full) { 'full' } else { 'slim' }
$out = Join-Path $root "dist\yinyidict-$Version-windows-x64-$kind"

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

if ($Full) {
  # 词库、模型、运行库。缺哪一样都不能算「完整」，所以逐个查而不是静默跳过。
  $need = @(
    @{ src = 'data\index'; dst = 'data\index'; what = '词库索引' }
    @{ src = 'models\kokoro-multi-lang-v1_1'; dst = 'models\kokoro-multi-lang-v1_1'; what = '语音模型' }
    @{
      src  = 'vendor\sherpa-onnx-v1.13.7-win-x64-shared-MT-Release'
      dst  = 'vendor\sherpa-onnx-v1.13.7-win-x64-shared-MT-Release'
      what = '推理运行库'
    }
  )
  foreach ($n in $need) {
    $s = Join-Path $root $n.src
    if (-not (Test-Path $s)) {
      throw "完整包缺$($n.what)：$s`n先跑 dict-build fetch，再跑 dict-build"
    }
    $d = Join-Path $out $n.dst
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $d) | Out-Null
    Write-Host "  收 $($n.what) …"
    Copy-Item $s $d -Recurse
  }
  # 再分发第三方数据就必须带上出处和授权
  Copy-Item (Join-Path $PSScriptRoot 'THIRD-PARTY.txt') $out
}

$zip = "$out.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Write-Host '  压缩中（完整包要几分钟）…'
Compress-Archive -Path "$out\*" -DestinationPath $zip -CompressionLevel Optimal

$mb = (Get-Item $zip).Length / 1MB
"打好了：$zip  ({0:N1} MB)" -f $mb
Get-ChildItem $out | ForEach-Object { "  $($_.Name)" }
