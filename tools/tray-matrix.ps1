# 窗口行为回归：跑真的程序，看真的反应。
#
#   pwsh tools/tray-matrix.ps1        （先 cargo build --release -p dict-app）
#
# 三部分：
#   一、托盘菜单九宫格 —— 三种窗口状态 × 三个菜单项。三个菜单项都在这里出过错，
#       而且每次只在其中一两种状态下发作，手点九次才碰上一次。
#   二、标题栏按钮 —— 最小化进任务栏、关闭收进托盘。「查完词点关闭没反应」
#       那个 bug 九宫格测不出来，它只在「藏起来又叫回来」之后才发作。
#   三、键盘与设置窗口 —— Esc 的三段式，以及「词典藏着时单独开设置窗口」
#       这条路上的三个要求：设置窗口自己得有帧、词典不许跟着上屏、
#       借去开窗口的主窗口要挪回原处。
#
# 菜单项走 WM_TRAY_ITEM 投递，不真的弹菜单：TrackPopupMenu 是模态的，
# 注入键鼠要求投递方是前台进程，脚本跑在后台给不了。到了 State::on
# 之后和真人点菜单是同一条路。
$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class T {
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string c, string n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr a, string c, string n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte sc, uint f, IntPtr e);
  [DllImport("user32.dll")] public static extern bool AllowSetForegroundWindow(uint pid);
  [DllImport("user32.dll")] public static extern bool GetMenuItemRect(IntPtr h, IntPtr m, uint i, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }

  // HWND_MESSAGE = -3 下的窗口不进 FindWindow/EnumWindows 的枚举，得指名去它下面找
  public static IntPtr Tray() { var h = FindWindowExW(new IntPtr(-3), IntPtr.Zero, "DictTrayClass", null); return h != IntPtr.Zero ? h : FindWindowW("DictTrayClass", null); }
  public static string Title(IntPtr h) { var s = new StringBuilder(256); GetWindowTextW(h, s, 256); return s.ToString(); }
  public static string Cls(IntPtr h) { var s = new StringBuilder(256); GetClassNameW(h, s, 256); return s.ToString(); }
  public static System.Collections.Generic.List<IntPtr> Windows(uint pid) {
    var o = new System.Collections.Generic.List<IntPtr>();
    EnumWindows((h, l) => { uint p; GetWindowThreadProcessId(h, out p); if (p == pid) o.Add(h); return true; }, IntPtr.Zero);
    return o;
  }
}
"@

# 从脚本自己的位置推工程根，别钉死绝对路径
$CWD = Split-Path -Parent $PSScriptRoot
$EXE = Join-Path $CWD 'target\release\dict.exe'
if (-not (Test-Path $EXE)) { throw "找不到 $EXE —— 先跑 cargo build --release -p dict-app" }
$MENU_X = 600; $MENU_Y = 500

function Kill-All { Get-Process dict -ErrorAction SilentlyContinue | Stop-Process -Force; Start-Sleep -Milliseconds 400 }

function Start-App {
  Kill-All
  $p = Start-Process -FilePath $EXE -WorkingDirectory $CWD -PassThru
  for ($i = 0; $i -lt 120; $i++) {
    Start-Sleep -Milliseconds 150
    $p.Refresh()
    if ($p.MainWindowHandle -ne [IntPtr]::Zero) { break }
  }
  Start-Sleep -Milliseconds 1200
  $p.Refresh()
  if ($p.MainWindowHandle -eq [IntPtr]::Zero) { throw '主窗口没出来' }
  $p
}

# 按窗口类找 eframe 的主窗口。
#
# 不能用 Process.MainWindowHandle：进程刚起来时它可能抓到 OpenGL 建上下文用的
# 临时窗口（__wglDummyWindowFodder），那个窗口随后会自己销毁 —— 拿它当主窗口，
# 测出来的「窗口没了」全是假象。winit 的窗口类叫 "Window Class"。
function Real-Main([int]$procId) {
  foreach ($h in [T]::Windows($procId)) {
    if ([T]::Cls($h) -eq 'Window Class') { return $h }
  }
  return [IntPtr]::Zero
}

function Settings-Hwnd([int]$procId, [IntPtr]$main) {
  foreach ($h in [T]::Windows($procId)) {
    if ($h -ne $main -and [T]::IsWindowVisible($h) -and [T]::Title($h) -like '*设置*') { return $h }
  }
  return [IntPtr]::Zero
}

function State-Of([int]$procId, [IntPtr]$main) {
  if (-not (Get-Process -Id $procId -ErrorAction SilentlyContinue)) { return 'gone' }
  if (-not [T]::IsWindow($main)) { return 'gone' }
  if (-not [T]::IsWindowVisible($main)) { return 'tray' }
  if ([T]::IsIconic($main)) { return 'minimized' }
  return 'front'
}

function Set-State([IntPtr]$main, [string]$want) {
  switch ($want) {
    'front'     { [void][T]::ShowWindow($main, 9); [void][T]::SetForegroundWindow($main) }
    'minimized' { [void][T]::ShowWindow($main, 9); Start-Sleep -Milliseconds 300; [void][T]::ShowWindow($main, 6) }
    'tray'      { [void][T]::ShowWindow($main, 9); Start-Sleep -Milliseconds 300
                  [void][T]::PostMessageW([T]::Tray(), 0x8001, [IntPtr]0, [IntPtr]0x0202) }  # 左键 = 收起
  }
  Start-Sleep -Milliseconds 1200
}

# 投递一个菜单项（1=显示词典 2=设置 3=退出）。
# 走 WM_TRAY_ITEM 而不是真的弹菜单：TrackPopupMenu 是模态的，注入键鼠要求
# 投递方是前台进程，脚本跑在后台给不了。到了 State::on 之后是同一条路。
function Click-Menu([int]$id) {
  if (-not [T]::PostMessageW([T]::Tray(), 0x8004, [IntPtr]0, [IntPtr]$id)) { throw 'PostMessage 失败' }
  Start-Sleep -Milliseconds 1200
  "        （投了菜单项 $id）"
}

# main = 点完之后主窗口该留在哪。点「设置」只该出设置窗口，词典**不许**跟着上屏 ——
# 最小化那一格是唯一会变的：最小化的窗口不出帧，子视口画不出来，所以先收进托盘。
$rows = @(
  @{ st = 'front';     item = '词典'; idx = 1; want = 'front' },
  @{ st = 'minimized'; item = '词典'; idx = 1; want = 'front' },
  @{ st = 'tray';      item = '词典'; idx = 1; want = 'front' },
  @{ st = 'front';     item = '设置'; idx = 2; want = 'settings'; main = 'front' },
  @{ st = 'minimized'; item = '设置'; idx = 2; want = 'settings'; main = 'tray' },
  @{ st = 'tray';      item = '设置'; idx = 2; want = 'settings'; main = 'tray' },
  @{ st = 'front';     item = '退出'; idx = 3; want = 'gone' },
  @{ st = 'minimized'; item = '退出'; idx = 3; want = 'gone' },
  @{ st = 'tray';      item = '退出'; idx = 3; want = 'gone' }
)
$name = @{ 'front' = '在前'; 'minimized' = '最小化'; 'tray' = '最小化在托盘'; 'gone' = '整个软件退出'; 'settings' = '设置窗口出现' }

# 目标/实际两栏对「设置」那三格还要带上主窗口的下落：只出设置窗口才算对，
# 词典跟着弹出来就是错 —— 那正是这一版要修的 bug。
function Label([string]$want, [string]$main) {
  if ($main) { "$($name[$want])·词典$($name[$main])" } else { $name[$want] }
}

"{0,-14} {1,-6} {2,-26} {3,-26} {4}" -f '主窗口', '点击', '目标反应', '实际反应', '判定'
'-' * 88
$bad = 0
foreach ($row in $rows) {
  $p = Start-App
  $main = $p.MainWindowHandle
  $script:TargetPid = [uint32]$p.Id
  Set-State $main $row.st
  $before = State-Of $p.Id $main
  if ($before -ne $row.st) { "  !! 摆状态失败：想要 $($row.st)，实际 $before" }
  Click-Menu $row.idx | Out-Null

  # 轮询到结果符合预期就停；一直不符合就等满 6 秒，报它最后的样子
  $got = ''; $gotMain = ''
  for ($k = 0; $k -lt 30; $k++) {
    $gotMain = State-Of $p.Id $main
    $got = $gotMain
    if ($got -ne 'gone' -and (Settings-Hwnd $p.Id $main) -ne [IntPtr]::Zero) { $got = 'settings' }
    if ($got -eq $row.want -and (-not $row.main -or $gotMain -eq $row.main)) { break }
    Start-Sleep -Milliseconds 200
  }
  $hit = ($got -eq $row.want) -and ((-not $row.main) -or ($gotMain -eq $row.main))
  $ok = if ($hit) { 'OK' } else { $bad++; '**错**' }
  $gotLabel = if ($row.main) { Label $got $gotMain } else { $name[$got] }
  "{0,-14} {1,-6} {2,-26} {3,-26} {4}" -f $name[$row.st], $row.item, (Label $row.want $row.main), $gotLabel, $ok
  Kill-All
}
'-' * 88
if ($bad -eq 0) { '九格全部符合预期' } else { "$bad 格不符合预期" }

# ═════════════ 二、标题栏按钮 ═════════════
''
'标题栏按钮'
'-' * 72
$bad2 = 0
function Check([string]$what, [string]$want, [string]$got) {
  $ok = if ($got -eq $want) { 'OK' } else { $script:bad2++; '**错**' }
  "{0,-34} 期望 {1,-10} 实际 {2,-10} {3}" -f $what, $want, $got, $ok
}

$p = Start-App
$script:TargetPid = [uint32]$p.Id
$main = Real-Main $p.Id
[void][T]::ShowWindow($main, 9); Start-Sleep -Milliseconds 800

# 最小化按钮 = 进任务栏，不是进托盘
[void][T]::PostMessageW($main, 0x0112, [IntPtr]0xF020, [IntPtr]0)   # SC_MINIMIZE
Start-Sleep -Milliseconds 1500
Check '最小化按钮' 'minimized' (State-Of $p.Id $main)

# 关闭按钮 = 收进托盘，不是退出
[void][T]::ShowWindow($main, 9); Start-Sleep -Milliseconds 800
[void][T]::PostMessageW($main, 0x0112, [IntPtr]0xF060, [IntPtr]0)   # SC_CLOSE
Start-Sleep -Milliseconds 1800
Check '关闭按钮' 'tray' (State-Of $p.Id $main)

# 叫回来，再关一次。
# 这一步才是重点：托盘那次「显示」是绕过 winit 直接 ShowWindow 的，
# 不把 winit 缓存的标志位拉回来对齐的话，第二次关闭会被它当成无事发生。
[void][T]::PostMessageW([T]::Tray(), 0x8004, [IntPtr]0, [IntPtr]1)  # 菜单「显示词典」
Start-Sleep -Milliseconds 1500
Check '托盘叫回来' 'front' (State-Of $p.Id $main)
[void][T]::PostMessageW($main, 0x0112, [IntPtr]0xF060, [IntPtr]0)
Start-Sleep -Milliseconds 1800
Check '叫回来之后再关一次' 'tray' (State-Of $p.Id $main)

Kill-All
'-' * 72
if ($bad2 -eq 0) { '标题栏按钮全部符合预期' } else { "$bad2 项不符合预期" }

# ═════════════ 三、键盘与设置窗口 ═════════════
#
# 键盘事件一律投进窗口的消息队列：脚本跑在后台，注入真键鼠要求自己是前台进程。
# 'a' 那一下要 keydown + char + keyup 三条齐全 —— winit 是按整串消息解析按键的，
# 光投一条 WM_CHAR 不会变成一次按键（试过，框里什么都没进去）。
function Key([IntPtr]$h, [int]$vk, [int]$ch) {
  [void][T]::PostMessageW($h, 0x0100, [IntPtr]$vk, [IntPtr]0)
  [void][T]::PostMessageW($h, 0x0102, [IntPtr]$ch, [IntPtr]0)
  [void][T]::PostMessageW($h, 0x0101, [IntPtr]$vk, [IntPtr]0)
}
function Esc-Key([IntPtr]$h) {
  [void][T]::PostMessageW($h, 0x0100, [IntPtr]0x1B, [IntPtr]0)
  [void][T]::PostMessageW($h, 0x0101, [IntPtr]0x1B, [IntPtr]0)
}
function Spot([IntPtr]$h) { $r = New-Object T+RECT; [void][T]::GetWindowRect($h, [ref]$r); "$($r.left),$($r.top)" }

''
'键盘与设置窗口'
'-' * 72
$bad3 = 0
function Check3([string]$what, [string]$want, [string]$got) {
  $ok = if ($got -eq $want) { 'OK' } else { $script:bad3++; '**错**' }
  "{0,-36} 期望 {1,-12} 实际 {2,-12} {3}" -f $what, $want, $got, $ok
}

$p = Start-App
$script:TargetPid = [uint32]$p.Id
$main = Real-Main $p.Id
[void][T]::ShowWindow($main, 9); [void][T]::SetForegroundWindow($main); Start-Sleep -Milliseconds 800
$spot = Spot $main

# 收进托盘，再从托盘菜单开设置
[void][T]::PostMessageW([T]::Tray(), 0x8001, [IntPtr]0, [IntPtr]0x0202); Start-Sleep -Milliseconds 1500
Check3 '左键单击收进托盘' 'tray' (State-Of $p.Id $main)
[void][T]::PostMessageW([T]::Tray(), 0x8004, [IntPtr]0, [IntPtr]2); Start-Sleep -Milliseconds 2500
$sw = Settings-Hwnd $p.Id $main
$got = if ($sw -ne [IntPtr]::Zero) { 'yes' } else { 'no' }
Check3 '托盘里点设置 = 设置窗口出现' 'yes' $got
Check3 '  ……词典自己还在托盘里' 'tray' (State-Of $p.Id $main)

# 主窗口藏着的时候子窗口照样得有帧：没帧就收不到关闭请求，点 × 会没反应
if ($sw -ne [IntPtr]::Zero) { [void][T]::PostMessageW($sw, 0x0010, [IntPtr]0, [IntPtr]0) }
Start-Sleep -Milliseconds 2000
$got = if ((Settings-Hwnd $p.Id $main) -eq [IntPtr]::Zero) { 'closed' } else { 'open' }
Check3 '关设置窗口（它自己得收到）' 'closed' $got

# 「借主窗口一帧」用完必须把它挪回原处，否则下次唤出词典会出现在屏幕外
[void][T]::PostMessageW([T]::Tray(), 0x8004, [IntPtr]0, [IntPtr]1); Start-Sleep -Milliseconds 1800
Check3 '再唤出词典：回到原来的位置' $spot (Spot $main)

# Esc 的三段式，这里测后两段
[void][T]::SetForegroundWindow($main); Start-Sleep -Milliseconds 600
Key $main 0x41 0x61; Start-Sleep -Milliseconds 900
Esc-Key $main; Start-Sleep -Milliseconds 1200
Check3 '框里有字：Esc 只清空，窗口留着' 'front' (State-Of $p.Id $main)
Esc-Key $main; Start-Sleep -Milliseconds 1800
Check3 '空框再按 Esc：收进托盘' 'tray' (State-Of $p.Id $main)

Kill-All
'-' * 72
if ($bad3 -eq 0) { '键盘与设置窗口全部符合预期' } else { "$bad3 项不符合预期" }

$total = $bad + $bad2 + $bad3
if ($total -eq 0) { ''; '全部通过' } else { ''; "共 $total 项不符合预期"; exit 1 }