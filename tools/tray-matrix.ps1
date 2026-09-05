# 窗口行为回归：跑真的程序，看真的反应。
#
#   pwsh tools/tray-matrix.ps1        （先 cargo build --release -p dict-app）
#
# 两部分：
#   一、托盘菜单九宫格 —— 三种窗口状态 × 三个菜单项。三个菜单项都在这里出过错，
#       而且每次只在其中一两种状态下发作，手点九次才碰上一次。
#   二、标题栏按钮 —— 最小化进任务栏、关闭收进托盘。「查完词点关闭没反应」
#       那个 bug 九宫格测不出来，它只在「藏起来又叫回来」之后才发作。
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

$EXE = "C:\Users\chen.chen2\Desktop\workdir\dictionary\target\release\dict.exe"
$CWD = "C:\Users\chen.chen2\Desktop\workdir\dictionary"
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

$rows = @(
  @{ st = 'front';     item = '词典'; idx = 1; want = 'front' },
  @{ st = 'minimized'; item = '词典'; idx = 1; want = 'front' },
  @{ st = 'tray';      item = '词典'; idx = 1; want = 'front' },
  @{ st = 'front';     item = '设置'; idx = 2; want = 'settings' },
  @{ st = 'minimized'; item = '设置'; idx = 2; want = 'settings' },
  @{ st = 'tray';      item = '设置'; idx = 2; want = 'settings' },
  @{ st = 'front';     item = '退出'; idx = 3; want = 'gone' },
  @{ st = 'minimized'; item = '退出'; idx = 3; want = 'gone' },
  @{ st = 'tray';      item = '退出'; idx = 3; want = 'gone' }
)
$name = @{ 'front' = '在前'; 'minimized' = '最小化'; 'tray' = '最小化在托盘'; 'gone' = '整个软件退出'; 'settings' = '设置窗口出现' }

"{0,-14} {1,-6} {2,-14} {3,-14} {4}" -f '主窗口', '点击', '目标反应', '实际反应', '判定'
'-' * 72
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
  $got = ''
  for ($k = 0; $k -lt 30; $k++) {
    $got = State-Of $p.Id $main
    if ($got -ne 'gone' -and (Settings-Hwnd $p.Id $main) -ne [IntPtr]::Zero) { $got = 'settings' }
    if ($got -eq $row.want) { break }
    Start-Sleep -Milliseconds 200
  }
  $ok = if ($got -eq $row.want) { 'OK' } else { $bad++; '**错**' }
  "{0,-14} {1,-6} {2,-14} {3,-14} {4}" -f $name[$row.st], $row.item, $name[$row.want], $name[$got], $ok
  Kill-All
}
'-' * 72
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
if ($bad + $bad2 -eq 0) { ''; '全部通过' } else { ''; "共 $($bad + $bad2) 项不符合预期"; exit 1 }