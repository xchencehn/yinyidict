# 把要装的东西收进 dist\payload，给 Inno Setup 打成安装程序。
#
#   pwsh tools/package.ps1                        只收文件
#   pwsh tools/package.ps1 -Version 0.1.2 -Iss    收完顺手打成 setup.exe
#
# 先 cargo build --release --workspace，再 dict-build fetch + dict-build
# 把词库准备好 —— 发布的是完整包，词库、模型、运行库全在里面。
#
# **这是在再分发第三方数据**，所以 payload 里必须有 THIRD-PARTY.txt：
# CC-CEDICT 是 CC BY-SA 3.0，署名和 ShareAlike 都是硬要求，不是礼貌。
param(
  [string]$Version = '0.0.0',
  [switch]$Iss
)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$rel = Join-Path $root 'target/release'
$payload = Join-Path $root 'dist/payload'

foreach ($f in @('dict.exe', 'dict-build.exe')) {
  if (-not (Test-Path (Join-Path $rel $f))) {
    throw "缺 $f —— 先跑 cargo build --release --workspace"
  }
}

if (Test-Path $payload) { Remove-Item $payload -Recurse -Force }
New-Item -ItemType Directory -Force -Path $payload | Out-Null

Copy-Item (Join-Path $rel 'dict.exe')       $payload
Copy-Item (Join-Path $rel 'dict-build.exe') $payload
Copy-Item (Join-Path $PSScriptRoot 'dist/README.txt') $payload
Copy-Item (Join-Path $PSScriptRoot 'THIRD-PARTY.txt') $payload

# 词库、模型、运行库。缺哪一样装出来都是残的，所以逐个查而不是静默跳过。
$need = @(
  @{ src = 'data/index'; what = '词库索引' }
  @{ src = 'models/kokoro-multi-lang-v1_1'; what = '语音模型' }
  @{ src = 'vendor/sherpa-onnx-v1.13.7-win-x64-shared-MT-Release'; what = '推理运行库' }
)
foreach ($n in $need) {
  $s = Join-Path $root $n.src
  if (-not (Test-Path $s)) {
    throw "缺$($n.what)：$s`n先跑 dict-build fetch，再跑 dict-build"
  }
  $d = Join-Path $payload $n.src
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $d) | Out-Null
  Write-Host "  收 $($n.what) …"
  Copy-Item $s $d -Recurse
}

$mb = (Get-ChildItem $payload -Recurse -File | Measure-Object Length -Sum).Sum / 1MB
"payload 收好了：$payload  ({0:N0} MB)" -f $mb

if (-not $Iss) { return }

$iscc = @(
  "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
  "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw '找不到 ISCC.exe —— 装一下 Inno Setup 6' }

# 安装向导的图标。build.rs 把 .ico 画在 OUT_DIR 里（不往版本库塞二进制资源），
# 这里捞一份出来给 Inno 用。
$isccArgs = @("/DAppVersion=$Version", "/DPayload=$payload")
$ico = Get-ChildItem (Join-Path $root 'target/release/build') -Recurse -Filter 'dict.ico' `
  -ErrorAction SilentlyContinue | Select-Object -First 1
if ($ico) {
  $dst = Join-Path $root 'dist/dict.ico'
  Copy-Item $ico.FullName $dst -Force
  $isccArgs += "/DSetupIcon=$dst"
}
else {
  Write-Host '  （没找到 dict.ico，安装向导用 Inno 的默认图标）'
}

Write-Host '  打安装程序（大头是模型和索引，要几分钟）…'
& $iscc @isccArgs (Join-Path $PSScriptRoot 'installer.iss')
if ($LASTEXITCODE -ne 0) { throw "ISCC 失败（$LASTEXITCODE）" }

$setup = Get-ChildItem (Join-Path $root 'dist') -Filter '*setup.exe' | Select-Object -First 1
"打好了：$($setup.FullName)  ({0:N1} MB)" -f ($setup.Length / 1MB)
