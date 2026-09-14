//! Win32 系统托盘 + 全局热键。
//!
//! 跑在自己的线程上，有自己的 message-only 窗口和消息循环 —— 不去碰 winit 那个。
//! 想复用 eframe 的窗口就得子类化它的 WndProc，那是在跟 eframe 抢消息，
//! 出问题很难查；另起一个只收消息的窗口干净得多，代价只是一个线程。
//!
//! 线程通过 channel 把事件送回界面，并 `request_repaint()` 把界面叫醒 ——
//! 窗口藏起来时 egui 是不出帧的，不叫醒就没人来处理这个事件。

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

// ─────────────────────────── Win32 ───────────────────────────

type Hwnd = *mut c_void;
type Hicon = *mut c_void;
type Hmenu = *mut c_void;
type Hbitmap = *mut c_void;
type Hinstance = *mut c_void;
type LResult = isize;
type WParam = usize;
type LParam = isize;

const WM_DESTROY: u32 = 0x0002;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_LBUTTONDBLCLK: u32 = 0x0203;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_HOTKEY: u32 = 0x0312;
const WM_CLOSE: u32 = 0x0010;
const WM_NULL: u32 = 0x0000;
/// 不进任务栏、不进 Alt+Tab。
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
/// 激活并还原：最小化的展开，藏起来的显示出来。
const SW_HIDE: i32 = 0;
const SW_RESTORE: i32 = 9;
/// 按原位显示，但不抢焦点。只在 [`tuck_away`] 里用，而且是在屏幕外用。
const SW_SHOWNOACTIVATE: i32 = 4;
const WM_APP: u32 = 0x8000;
/// 托盘图标的回调消息。
const WM_TRAY: u32 = WM_APP + 1;
/// 界面让我们换热键：wParam = 修饰键，lParam = 虚拟键码。
const WM_SET_HOTKEY: u32 = WM_APP + 2;
/// 界面让我们收摊。
const WM_QUIT_TRAY: u32 = WM_APP + 3;
/// 自动化测试用：直接投递一个菜单项，跳过弹菜单那一步。
///
/// `lParam` 取 `ID_SHOW` / `ID_SETTINGS` / `ID_QUIT`。之所以要有它：
/// `TrackPopupMenu` 是模态的，注入的键鼠事件要求投递方是前台进程，
/// 脚本跑在后台时给不了 —— 于是「托盘菜单在各种窗口状态下的反应」这张
/// 九宫格表就没法自动跑。而那张表恰恰是反复出过错的地方。
/// 走的是和真人点菜单**同一个** [`State::on`]，只少一次弹菜单。
const WM_TRAY_ITEM: u32 = WM_APP + 4;

const NIM_ADD: u32 = 0;
const NIM_DELETE: u32 = 2;
const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;

const MF_STRING: u32 = 0x0000;
const MF_SEPARATOR: u32 = 0x0800;
const TPM_RIGHTBUTTON: u32 = 0x0002;
const TPM_RETURNCMD: u32 = 0x0100;

const GWLP_USERDATA: i32 = -21;
const HOTKEY_ID: i32 = 0xD1C7;
const IMAGE_ICON: u32 = 1;
const LR_DEFAULTSIZE: u32 = 0x0040;

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    a: u32,
    b: u16,
    c: u16,
    d: [u8; 8],
}

#[repr(C)]
struct NotifyIconDataW {
    cb_size: u32,
    hwnd: Hwnd,
    u_id: u32,
    u_flags: u32,
    u_callback_message: u32,
    h_icon: Hicon,
    sz_tip: [u16; 128],
    dw_state: u32,
    dw_state_mask: u32,
    sz_info: [u16; 256],
    u_version: u32,
    sz_info_title: [u16; 64],
    dw_info_flags: u32,
    guid_item: Guid,
    h_balloon_icon: Hicon,
}

#[repr(C)]
struct WndClassExW {
    cb_size: u32,
    style: u32,
    lpfn_wnd_proc: Option<unsafe extern "system" fn(Hwnd, u32, WParam, LParam) -> LResult>,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: Hinstance,
    h_icon: Hicon,
    h_cursor: *mut c_void,
    hbr_background: *mut c_void,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
    h_icon_sm: Hicon,
}

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// `WINDOWPLACEMENT`。要它只为一件事：把「还原之后摆在哪」读出来、改掉、再放回去。
#[repr(C)]
struct WindowPlacement {
    length: u32,
    flags: u32,
    show_cmd: u32,
    min_pos: Point,
    max_pos: Point,
    normal: Rect,
}

#[repr(C)]
struct Msg {
    hwnd: Hwnd,
    message: u32,
    w_param: WParam,
    l_param: LParam,
    time: u32,
    pt: Point,
}

#[repr(C)]
struct IconInfo {
    f_icon: i32,
    x_hotspot: u32,
    y_hotspot: u32,
    hbm_mask: Hbitmap,
    hbm_color: Hbitmap,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(c: *const WndClassExW) -> u16;
    fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: Hwnd,
        menu: Hmenu,
        inst: Hinstance,
        param: *mut c_void,
    ) -> Hwnd;
    fn DefWindowProcW(h: Hwnd, m: u32, w: WParam, l: LParam) -> LResult;
    fn DestroyWindow(h: Hwnd) -> i32;
    fn GetMessageW(msg: *mut Msg, h: Hwnd, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> LResult;
    fn PostQuitMessage(code: i32);
    fn PostMessageW(h: Hwnd, m: u32, w: WParam, l: LParam) -> i32;
    fn SetWindowLongPtrW(h: Hwnd, idx: i32, v: isize) -> isize;
    fn GetWindowLongPtrW(h: Hwnd, idx: i32) -> isize;
    fn RegisterHotKey(h: Hwnd, id: i32, modifiers: u32, vk: u32) -> i32;
    fn UnregisterHotKey(h: Hwnd, id: i32) -> i32;
    fn CreatePopupMenu() -> Hmenu;
    fn DestroyMenu(m: Hmenu) -> i32;
    fn AppendMenuW(m: Hmenu, flags: u32, id: usize, item: *const u16) -> i32;
    fn TrackPopupMenu(
        m: Hmenu,
        flags: u32,
        x: i32,
        y: i32,
        reserved: i32,
        h: Hwnd,
        rect: *const c_void,
    ) -> i32;
    fn GetCursorPos(p: *mut Point) -> i32;
    fn SetForegroundWindow(h: Hwnd) -> i32;
    fn ShowWindow(h: Hwnd, cmd: i32) -> i32;
    fn IsWindowVisible(h: Hwnd) -> i32;
    fn IsIconic(h: Hwnd) -> i32;
    fn GetWindowPlacement(h: Hwnd, p: *mut WindowPlacement) -> i32;
    fn SetWindowPlacement(h: Hwnd, p: *const WindowPlacement) -> i32;
    fn MessageBoxW(h: Hwnd, text: *const u16, cap: *const u16, ty: u32) -> i32;
    fn CreateIconIndirect(info: *const IconInfo) -> Hicon;
    fn LoadImageW(
        inst: Hinstance,
        name: *const u16,
        ty: u32,
        cx: i32,
        cy: i32,
        load: u32,
    ) -> *mut c_void;
}

#[repr(C)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_ppm: i32,
    y_ppm: i32,
    clr_used: u32,
    clr_important: u32,
}

const BI_RGB: u32 = 0;
const DIB_RGB_COLORS: u32 = 0;

#[link(name = "gdi32")]
extern "system" {
    fn CreateBitmap(w: i32, h: i32, planes: u32, bpp: u32, bits: *const c_void) -> Hbitmap;
    fn CreateDIBSection(
        dc: *mut c_void,
        bmi: *const BitmapInfoHeader,
        usage: u32,
        bits: *mut *mut c_void,
        section: *mut c_void,
        offset: u32,
    ) -> Hbitmap;
    fn DeleteObject(o: *mut c_void) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn GetDC(h: Hwnd) -> *mut c_void;
    fn ReleaseDC(h: Hwnd, dc: *mut c_void) -> i32;
    fn DestroyIcon(i: Hicon) -> i32;
    fn SendMessageW(h: Hwnd, m: u32, w: WParam, l: LParam) -> LResult;
}

#[link(name = "shell32")]
extern "system" {
    fn Shell_NotifyIconW(msg: u32, data: *const NotifyIconDataW) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> Hinstance;
    fn GetLastError() -> u32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ─────────────────────────── 热键 ───────────────────────────

/// 全局热键。`mods` 是 Win32 的 MOD_* 位掩码，`vk` 是虚拟键码。
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;
/// 按住不放时不重复触发。
const MOD_NOREPEAT: u32 = 0x4000;

impl Default for Hotkey {
    fn default() -> Self {
        // 注册全局热键等于从全系统抢走这个组合，而 Alt+数字有些输入法在用 ——
        // 这是用户明确指定的，设置里随时能改。
        Hotkey { mods: MOD_ALT, vk: b'1' as u32 }
    }
}

/// 首选注册不上时依次试这些。全局热键被占是常事，
/// 与其让这个功能直接死掉，不如换一个并明确告诉用户换成了什么。
pub const FALLBACKS: &[Hotkey] = &[
    Hotkey { mods: MOD_ALT, vk: b'1' as u32 },
    Hotkey { mods: MOD_CONTROL | MOD_SHIFT, vk: b'D' as u32 },
    Hotkey { mods: MOD_CONTROL | MOD_ALT, vk: b'Z' as u32 },
    Hotkey { mods: MOD_CONTROL | MOD_SHIFT, vk: 0x20 },
    Hotkey { mods: MOD_ALT | MOD_SHIFT, vk: b'D' as u32 },
    Hotkey { mods: MOD_CONTROL | MOD_ALT, vk: b'Q' as u32 },
];

impl Hotkey {
    /// 人能读的写法，如 `Ctrl + Alt + D`。
    pub fn label(&self) -> String {
        let mut p = Vec::new();
        if self.mods & MOD_CONTROL != 0 {
            p.push("Ctrl".to_string());
        }
        if self.mods & MOD_ALT != 0 {
            p.push("Alt".to_string());
        }
        if self.mods & MOD_SHIFT != 0 {
            p.push("Shift".to_string());
        }
        if self.mods & MOD_WIN != 0 {
            p.push("Win".to_string());
        }
        p.push(vk_name(self.vk));
        p.join(" + ")
    }

    /// 至少要有一个修饰键，否则会把普通打字全截走。
    pub fn is_valid(&self) -> bool {
        self.mods & (MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_WIN) != 0 && self.vk != 0
    }
}

pub fn vk_name(vk: u32) -> String {
    match vk {
        0x20 => "Space".into(),
        0x0D => "Enter".into(),
        0x1B => "Esc".into(),
        0x70..=0x7B => format!("F{}", vk - 0x6F),
        0x30..=0x39 => ((vk as u8) as char).to_string(),
        0x41..=0x5A => ((vk as u8) as char).to_string(),
        other => format!("VK{other:#04X}"),
    }
}

// ─────────────────────────── 事件与决策 ───────────────────────────

/// 用户的意图。左键、热键、菜单三个入口产生的原始事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// 热键或左键单击：显示则隐藏，隐藏则显示。
    Toggle,
    /// 菜单里的「显示词典」。
    Show,
    Settings,
    Quit,
}

/// 主窗口在托盘看来的状态。
///
/// 「不在眼前」有**两种**，而且不是一回事：收进托盘是窗口被隐藏了，
/// 最小化时窗口仍然是 visible 的，只是缩进了任务栏。把两者混为一谈会同时
/// 弄坏三个菜单项，见 [`decide`] 上的表。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WinState {
    /// 在眼前。
    Front,
    /// 最小化进任务栏。窗口还在，只是不出帧。
    Minimized,
    /// 收进托盘。
    Hidden,
}

/// 该做什么。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    /// 什么也不做。
    Keep,
    /// 把主窗口叫到眼前；`focus` = 顺带把焦点也给它。
    Summon { focus: bool },
    /// 收进托盘。
    Dismiss,
    /// 打开设置窗口，**主窗口一律不搬上屏幕**。
    ///
    /// `tuck` = 打开之前先把主窗口收进托盘。只有它最小化时才为真，原因是
    /// 最小化的窗口 eframe 不给它出帧（`info.visible()` 由 `IsIconic` 推出来），
    /// 而设置是它的 immediate 子视口，没有那一帧就画不出来。收进托盘的窗口
    /// 反倒照常出帧 —— Win32 不给隐藏窗口发 `WM_PAINT`，但 eframe 会绕过
    /// 消息循环直接来一帧（`is_invisible_or_minimized` 那条路）。
    /// 见 [`tuck_away`]：收的过程在屏幕外完成，看不见闪。
    OpenSettings { tuck: bool },
    /// 真的退出。
    Quit,
}

/// 托盘事件 × 窗口状态 → 动作。
///
/// 抽成纯函数只为一件事：这张表整个能上单元测试。三个菜单项曾经**全部**
/// 在这里出过错，而且每次都只在其中一两种窗口状态下发作 —— 手点九次才发现
/// 一次的 bug，值得让它在 `cargo test` 里现形。
///
/// | 窗口 | 词典 | 设置 | 退出 |
/// |---|---|---|---|
/// | 在眼前 | 保持 | 只出设置窗口，主窗口原样留着 | 退出 |
/// | 最小化 | 主窗口出现 | 只出设置窗口，主窗口收进托盘 | 退出 |
/// | 在托盘 | 主窗口出现 | 只出设置窗口，主窗口继续藏着 | 退出 |
///
/// 左键单击（`Toggle`）另算：在眼前时收起，其余两种状态都是叫出来。
pub fn decide(ev: TrayEvent, st: WinState) -> Act {
    match ev {
        // 左键是开关：只有真在眼前才该收起来。把最小化当成「在眼前」的话，
        // 点它等于去隐藏一个已经看不见的窗口 —— 表现就是「没反应」。
        TrayEvent::Toggle => match st {
            WinState::Front => Act::Dismiss,
            _ => Act::Summon { focus: true },
        },
        // 菜单里的「显示词典」不是开关，已经在眼前就什么都不做
        TrayEvent::Show => match st {
            WinState::Front => Act::Keep,
            _ => Act::Summon { focus: true },
        },
        // 点「设置」就只该出设置窗口，不该把词典一起搬上屏幕。
        //
        // 设置是主窗口的 immediate 子视口，由主窗口那一帧驱动 —— 但**只有
        // 最小化**会真的把那一帧停掉，收进托盘的窗口 eframe 照样给帧。
        // 所以只有最小化这一种要先把主窗口挪个地方（收进托盘，见 tuck_away），
        // 其余两种一根手指都不用动主窗口。
        TrayEvent::Settings => Act::OpenSettings { tuck: st == WinState::Minimized },
        TrayEvent::Quit => Act::Quit,
    }
}

/// 直接问 Win32 要窗口状态。
///
/// **不能问界面自己记的那个 `visible`**：窗口最小化时 eframe 根本不跑
/// 那一轮循环（`sleep_if_invisible_or_minimized` 之后就 return 了），
/// 它记的东西可能是几秒前的。托盘线程有自己的消息循环，随时都醒着。
pub fn win_state(hwnd: usize) -> WinState {
    if hwnd == 0 {
        return WinState::Front;
    }
    // SAFETY: 只读窗口状态，hwnd 无效时两个函数都返回 0。
    unsafe {
        let h = hwnd as Hwnd;
        if IsWindowVisible(h) == 0 {
            WinState::Hidden
        } else if IsIconic(h) != 0 {
            WinState::Minimized
        } else {
            WinState::Front
        }
    }
}

/// 把窗口动作落到实处，在**托盘线程**里直接做掉。
///
/// > **踩过的坑，这是最深的一个。**
/// > 原来是发 `ViewportCommand::Minimized(false)` / `Visible(true)` 让界面去做。
/// > 收进托盘时这条路走得通 —— eframe 对隐藏的窗口会走 `update_logic_only`，
/// > 界面逻辑照常跑。但**最小化时它连这一轮都不跑**：没有 `WM_PAINT`，
/// > 就没有 `RedrawRequested`，`logic()` 一次都不会被调用，事件就一直躺在
/// > 通道里。表现是托盘菜单点了完全没反应。
/// >
/// > 所以窗口的显隐必须由托盘线程自己动手。它有独立的消息循环，
/// > 主窗口最小化不影响它。界面那边只需要跟着同步一下自己的记账。
fn apply(hwnd: usize, act: Act) {
    if hwnd == 0 {
        return;
    }
    // SAFETY: hwnd 由 eframe 创建，在本进程存活期间一直有效。
    unsafe {
        let h = hwnd as Hwnd;
        match act {
            Act::Keep | Act::Quit => {}
            Act::Summon { focus } => {
                // SW_RESTORE 一条覆盖两种状态：最小化的展开，藏起来的显示出来
                ShowWindow(h, SW_RESTORE);
                if focus {
                    SetForegroundWindow(h);
                }
            }
            // 点设置只出设置窗口，主窗口不动。最小化那一种例外：它不出帧，
            // 子视口就画不出来，所以先无声无息地收进托盘。
            Act::OpenSettings { tuck } => {
                if tuck {
                    tuck_away(h);
                }
            }
            Act::Dismiss => {
                ShowWindow(h, SW_HIDE);
            }
        }
    }
}

/// 把最小化的窗口收进托盘，**过程在屏幕外完成**。
///
/// 为什么不能只 `ShowWindow(SW_HIDE)`：隐藏不会解掉最小化，`IsIconic` 仍然是
/// 真。而 eframe 判断「这一帧要不要跑界面」看的正是最小化
/// （`ViewportInfo::visible()` 由 `minimized` / `occluded` 推出来，
/// 跟窗口可见性无关），于是主窗口一直不出帧，它的 immediate 子视口
/// —— 设置窗口 —— 也就永远画不出来。
///
/// 为什么不能先 `SW_RESTORE` 再 `SW_HIDE`：那会当着用户的面闪一下词典窗口，
/// 而这次改动的全部目的就是别闪。
///
/// 所以走 `WINDOWPLACEMENT`：先把「还原之后摆在哪」改到屏幕外，就地展开
/// （`SW_SHOWNOACTIVATE`，不抢焦点），立刻隐藏，再把原来的位置写回去。
/// 出来的状态和从「在眼前」点关闭一模一样：隐藏、非最小化、位置没动,
/// 下次 `SW_RESTORE` 回到原地。
///
/// # Safety
/// `hwnd` 必须是本进程有效的窗口句柄。
unsafe fn tuck_away(h: Hwnd) {
    let mut wp: WindowPlacement = std::mem::zeroed();
    wp.length = std::mem::size_of::<WindowPlacement>() as u32;
    if GetWindowPlacement(h, &mut wp) == 0 {
        // 读不到就退回最朴素的一手：窗口会留在最小化状态，设置窗口开不出来，
        // 但至少没把窗口搞坏
        ShowWindow(h, SW_HIDE);
        return;
    }
    let keep = wp.normal;
    let (w, t) = (keep.right - keep.left, keep.bottom - keep.top);
    // 屏幕外的一角。−32000 是 Win32 自己给最小化窗口用的坐标量级
    wp.normal = Rect { left: -32000, top: -32000, right: -32000 + w, bottom: -32000 + t };
    wp.show_cmd = SW_SHOWNOACTIVATE as u32;
    SetWindowPlacement(h, &wp);
    ShowWindow(h, SW_HIDE);
    wp.normal = keep;
    wp.show_cmd = SW_HIDE as u32;
    SetWindowPlacement(h, &wp);
}

const ID_SHOW: usize = 1;
const ID_SETTINGS: usize = 2;
const ID_QUIT: usize = 3;

// ─────────────────────────── 线程状态 ───────────────────────────

struct State {
    tx: Sender<Act>,
    ctx: egui::Context,
    hwnd: Hwnd,
    /// 主窗口的句柄。界面拿到之后填进来，托盘线程靠它读窗口状态、动窗口。
    main: Arc<AtomicUsize>,
    /// 托盘图标句柄，线程退出时释放。
    icon: Hicon,
}

impl State {
    /// 收到用户意图：**当场把窗口动作做掉**，再把做了什么告诉界面。
    ///
    /// 顺序很重要。窗口最小化时界面那一轮循环根本不跑，指望它去动窗口
    /// 就是没反应。托盘线程自己做完，窗口一亮界面自然就醒了。
    fn on(&self, ev: TrayEvent) {
        let main = self.main.load(Ordering::Relaxed);
        let act = decide(ev, win_state(main));
        apply(main, act);
        if act == Act::Keep {
            return;
        }
        let _ = self.tx.send(act);
        if act == Act::Quit {
            // 退出要走界面：设置得存盘。而最小化的窗口不出帧，光
            // request_repaint 叫不醒它 —— 发个 WM_CLOSE 才是硬通货，
            // 窗口消息不受最小化影响，winit 会把它变成一次真正的循环。
            // SAFETY: main 非 0 时是 eframe 创建的窗口，本进程内一直有效。
            unsafe {
                if main != 0 {
                    PostMessageW(main as Hwnd, WM_CLOSE, 0, 0);
                }
            }
        }
        self.ctx.request_repaint();
    }
}

pub struct Tray {
    rx: Receiver<Act>,
    hwnd: usize,
    main: Arc<AtomicUsize>,
    /// 真正注册上的那个热键。和请求的不一致说明走了回退，`None` 表示全被占。
    active: Option<Hotkey>,
}

impl Tray {
    /// 起托盘线程。图标取自 [`crate::icon`]，热键注册失败不算致命 ——
    /// 常见原因是别的软件占了同一组合，界面会把这个情况显示出来。
    pub fn start(ctx: egui::Context, hotkey: Hotkey) -> std::io::Result<Tray> {
        let (tx, rx) = std::sync::mpsc::channel();
        let main = Arc::new(AtomicUsize::new(0));
        let main_for_thread = Arc::clone(&main);
        let (ready_tx, ready_rx) =
            std::sync::mpsc::channel::<Result<(usize, Option<Hotkey>), String>>();

        std::thread::Builder::new().name("tray".into()).spawn(move || {
            // SAFETY: 整段都在本线程内完成窗口的创建、使用和销毁；
            // 每个 Win32 调用的签名都照 MSDN 声明。
            unsafe {
                let inst = GetModuleHandleW(std::ptr::null());
                let class = wide("DictTrayClass");
                let wc = WndClassExW {
                    cb_size: std::mem::size_of::<WndClassExW>() as u32,
                    style: 0,
                    lpfn_wnd_proc: Some(wnd_proc),
                    cb_cls_extra: 0,
                    cb_wnd_extra: 0,
                    h_instance: inst,
                    h_icon: std::ptr::null_mut(),
                    h_cursor: std::ptr::null_mut(),
                    hbr_background: std::ptr::null_mut(),
                    lpsz_menu_name: std::ptr::null(),
                    lpsz_class_name: class.as_ptr(),
                    h_icon_sm: std::ptr::null_mut(),
                };
                let atom = RegisterClassExW(&wc);
                if atom == 0 {
                    // 1410 = ERROR_CLASS_ALREADY_EXISTS，那种情况可以继续
                    let err = GetLastError();
                    if err != 1410 {
                        let _ = ready_tx.send(Err(format!("RegisterClassExW 失败 err={err}")));
                        return;
                    }
                }

                // 顶层窗口，但不带 WS_VISIBLE，所以永远不上屏；
                // WS_EX_TOOLWINDOW 让它不进任务栏和 Alt+Tab。
                //
                // > **踩过的坑**：这里本来挂在 `HWND_MESSAGE` 下（只收消息的
                // > 窗口）。那样弹出的托盘菜单是坏的 —— `TrackPopupMenu` 要求
                // > 菜单的属主是前台窗口，而 message-only 窗口**当不了前台**，
                // > `SetForegroundWindow` 直接失败。后果是菜单点了不选中、
                // > 点别处也不消失。改成不显示的顶层窗口就都好了。
                let hwnd = CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    class.as_ptr(),
                    wide("词典").as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    inst,
                    std::ptr::null_mut(),
                );
                if hwnd.is_null() {
                    let _ =
                        ready_tx.send(Err(format!("CreateWindowExW 失败 err={}", GetLastError())));
                    return;
                }

                let icon = make_hicon(32);
                let state =
                    Box::into_raw(Box::new(State { tx, ctx, hwnd, main: main_for_thread, icon }));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);

                if !add_tray_icon(hwnd, icon) {
                    eprintln!("托盘：Shell_NotifyIconW 失败 err={}", GetLastError());
                }
                let active = register_first_free(hwnd, hotkey);
                let _ = ready_tx.send(Ok((hwnd as usize, active)));

                let mut msg: Msg = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                UnregisterHotKey(hwnd, HOTKEY_ID);
                remove_tray_icon(hwnd);
                destroy_icon((*state).icon);
                drop(Box::from_raw(state));
            }
        })?;

        let (hwnd, active) = ready_rx
            .recv()
            .map_err(|_| std::io::Error::other("托盘线程没有回应"))?
            .map_err(std::io::Error::other)?;
        Ok(Tray { rx, hwnd, main, active })
    }

    pub fn try_recv(&self) -> Option<Act> {
        self.rx.try_recv().ok()
    }

    /// 把主窗口句柄交给托盘线程。拿不到句柄之前托盘只能干瞪眼，
    /// 所以界面一拿到就得调这个。
    pub fn set_main_hwnd(&self, hwnd: usize) {
        self.main.store(hwnd, Ordering::Relaxed);
    }

    /// 当前真正生效的热键。`None` = 全被占用。
    pub fn active_hotkey(&self) -> Option<Hotkey> {
        self.active
    }

    /// 换热键。旧的先注销，新的注册不上就只是没有热键，不影响别的。
    pub fn set_hotkey(&self, hk: Hotkey) {
        // SAFETY: hwnd 由 start() 创建，托盘线程退出前一直有效；
        // PostMessage 是跨线程安全的。
        unsafe {
            PostMessageW(self.hwnd as Hwnd, WM_SET_HOTKEY, hk.mods as WParam, hk.vk as LParam);
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // SAFETY: 同上；线程收到后自行清理图标和热键。
        unsafe {
            PostMessageW(self.hwnd as Hwnd, WM_QUIT_TRAY, 0, 0);
        }
    }
}

/// 先试 `want`，不行就顺着 [`FALLBACKS`] 往下找第一个能注册上的。
///
/// # Safety
/// 只在托盘线程里调用，`hwnd` 必须是该线程创建的窗口。
unsafe fn register_first_free(hwnd: Hwnd, want: Hotkey) -> Option<Hotkey> {
    let mut tried = Vec::new();
    for hk in std::iter::once(want).chain(FALLBACKS.iter().copied()) {
        if !hk.is_valid() || tried.contains(&hk) {
            continue;
        }
        tried.push(hk);
        if RegisterHotKey(hwnd, HOTKEY_ID, hk.mods | MOD_NOREPEAT, hk.vk) != 0 {
            if hk != want {
                eprintln!("托盘：{} 被占用，改用 {}", want.label(), hk.label());
            }
            return Some(hk);
        }
    }
    eprintln!("托盘：{} 及全部备选都被占用，热键未启用", want.label());
    None
}

/// # Safety
/// 由 Win32 消息泵调用；`hwnd` 的 `GWLP_USERDATA` 里存着 `*mut State`。
unsafe extern "system" fn wnd_proc(hwnd: Hwnd, msg: u32, w: WParam, l: LParam) -> LResult {
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
    if state.is_null() {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let st = &*state;

    match msg {
        WM_TRAY => {
            match l as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => st.on(TrayEvent::Toggle),
                WM_RBUTTONUP => show_menu(st),
                _ => {}
            }
            0
        }
        WM_HOTKEY if w as i32 == HOTKEY_ID => {
            st.on(TrayEvent::Toggle);
            0
        }
        WM_SET_HOTKEY => {
            UnregisterHotKey(hwnd, HOTKEY_ID);
            let hk = Hotkey { mods: w as u32, vk: l as u32 };
            if hk.is_valid() && RegisterHotKey(hwnd, HOTKEY_ID, hk.mods | MOD_NOREPEAT, hk.vk) == 0
            {
                eprintln!("托盘：{} 注册失败（多半被别的软件占了）", hk.label());
            }
            0
        }
        WM_TRAY_ITEM => {
            match l as usize {
                ID_SHOW => st.on(TrayEvent::Show),
                ID_SETTINGS => st.on(TrayEvent::Settings),
                ID_QUIT => st.on(TrayEvent::Quit),
                _ => {}
            }
            0
        }
        WM_QUIT_TRAY => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

/// # Safety
/// 只在托盘线程的 WndProc 里调用。
unsafe fn show_menu(st: &State) {
    let menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING, ID_SHOW, wide("显示词典").as_ptr());
    AppendMenuW(menu, MF_STRING, ID_SETTINGS, wide("设置").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(menu, MF_STRING, ID_QUIT, wide("退出").as_ptr());

    let mut pt = Point { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    // 不先抢前台的话，菜单在别处点击时不会消失 —— Win32 的老规矩
    SetForegroundWindow(st.hwnd);
    let cmd = TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_RETURNCMD,
        pt.x,
        pt.y,
        0,
        st.hwnd,
        std::ptr::null(),
    );
    DestroyMenu(menu);
    // KB135788：菜单收起来之后给属主补一条空消息，否则它的菜单状态清不干净
    PostMessageW(st.hwnd, WM_NULL, 0, 0);

    match cmd as usize {
        ID_SHOW => st.on(TrayEvent::Show),
        ID_SETTINGS => st.on(TrayEvent::Settings),
        ID_QUIT => st.on(TrayEvent::Quit),
        _ => {}
    }
}

/// 把 [`crate::icon`] 画的 RGBA 变成 HICON。
///
/// 色位图必须走 `CreateDIBSection` 而不是 `CreateBitmap`：后者建的是**设备相关
/// 位图**，做图标时 alpha 通道会被丢掉，托盘里就是一块空白或全黑。
/// 负的 height 表示自上而下，正好对上我们缓冲区的行序。
///
/// # Safety
/// 返回的 HICON 归调用方所有（用完 `DestroyIcon`）；两个中间位图在
/// `CreateIconIndirect` 复制过内容之后即可释放。
pub(crate) unsafe fn make_hicon(size: u32) -> Hicon {
    let rgba = crate::icon::rgba(size);
    let bmi = BitmapInfoHeader {
        size: std::mem::size_of::<BitmapInfoHeader>() as u32,
        width: size as i32,
        height: -(size as i32),
        planes: 1,
        bit_count: 32,
        compression: BI_RGB,
        size_image: 0,
        x_ppm: 0,
        y_ppm: 0,
        clr_used: 0,
        clr_important: 0,
    };

    let dc = GetDC(std::ptr::null_mut());
    let mut bits: *mut c_void = std::ptr::null_mut();
    let color = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
    ReleaseDC(std::ptr::null_mut(), dc);
    if color.is_null() || bits.is_null() {
        return std::ptr::null_mut();
    }

    // RGBA → BGRA，并按 alpha 预乘：DWM 合成图标时按预乘 alpha 解读，
    // 不乘的话半透明边缘会发白。
    let dst = std::slice::from_raw_parts_mut(bits as *mut u8, rgba.len());
    for (d, srgb) in dst.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
        let a = srgb[3] as u32;
        d[0] = (srgb[2] as u32 * a / 255) as u8; // B
        d[1] = (srgb[1] as u32 * a / 255) as u8; // G
        d[2] = (srgb[0] as u32 * a / 255) as u8; // R
        d[3] = srgb[3];
    }

    // 32 位色图靠 alpha 决定透明，掩码全 0 即可 —— 但 ICONINFO 要求必须有一个
    let stride = (size.div_ceil(32) * 4) as usize;
    let mask_bytes = vec![0u8; stride * size as usize];
    let mask = CreateBitmap(size as i32, size as i32, 1, 1, mask_bytes.as_ptr() as *const c_void);

    let info = IconInfo { f_icon: 1, x_hotspot: 0, y_hotspot: 0, hbm_mask: mask, hbm_color: color };
    let icon = CreateIconIndirect(&info);
    DeleteObject(color);
    DeleteObject(mask);
    icon
}

const WM_SETICON: u32 = 0x0080;
const ICON_SMALL: usize = 0;
const ICON_BIG: usize = 1;

/// 弹一个系统对话框。
///
/// 这是个 GUI 子系统的程序，**没有控制台**：启动失败时 `eprintln!` 谁也看不见，
/// 双击之后就是「什么都没发生」。第一次运行还没准备词库的人，见到的正是这一幕。
pub fn alert(title: &str, text: &str) {
    // SAFETY: 两个字符串都以 NUL 结尾，MB_OK | MB_ICONWARNING。
    unsafe {
        MessageBoxW(std::ptr::null_mut(), wide(text).as_ptr(), wide(title).as_ptr(), 0x0000_0030);
    }
}

/// 给主窗口挂上标题栏和任务栏图标。
///
/// winit 的 `with_icon` 有时不足以让任务栏用上我们的图标，直接发 `WM_SETICON`
/// 是最直接的一手：小图标给标题栏，大图标给任务栏和 Alt+Tab。
pub fn set_window_icon(hwnd: usize) {
    if hwnd == 0 {
        return;
    }
    // SAFETY: hwnd 由 eframe 创建且在本进程存活期间有效；
    // 图标交给窗口后由系统持有，进程退出时统一回收。
    unsafe {
        let h = hwnd as Hwnd;
        let small = make_hicon(16);
        let big = make_hicon(32);
        if !small.is_null() {
            SendMessageW(h, WM_SETICON, ICON_SMALL, small as LParam);
        }
        if !big.is_null() {
            SendMessageW(h, WM_SETICON, ICON_BIG, big as LParam);
        }
    }
}

/// 释放一个由 [`make_hicon`] 生成的图标。
///
/// # Safety
/// `icon` 必须来自 `make_hicon` 且尚未被销毁。
pub(crate) unsafe fn destroy_icon(icon: Hicon) {
    if !icon.is_null() {
        DestroyIcon(icon);
    }
}

/// # Safety
/// `hwnd` 必须是托盘线程创建的那个窗口。
unsafe fn add_tray_icon(hwnd: Hwnd, icon: Hicon) -> bool {
    let mut data: NotifyIconDataW = std::mem::zeroed();
    data.cb_size = std::mem::size_of::<NotifyIconDataW>() as u32;
    data.hwnd = hwnd;
    data.u_id = 1;
    data.u_flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.u_callback_message = WM_TRAY;
    data.h_icon = icon;
    if data.h_icon.is_null() {
        // 退一步用系统默认应用图标，总比托盘里一片空白强
        data.h_icon = LoadImageW(
            std::ptr::null_mut(),
            32512 as *const u16, // IDI_APPLICATION
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE,
        ) as Hicon;
    }
    for (i, c) in wide("词典").iter().enumerate().take(127) {
        data.sz_tip[i] = *c;
    }
    Shell_NotifyIconW(NIM_ADD, &data) != 0
}

/// # Safety
/// `hwnd` 必须是当初注册图标用的那个窗口。
unsafe fn remove_tray_icon(hwnd: Hwnd) {
    let mut data: NotifyIconDataW = std::mem::zeroed();
    data.cb_size = std::mem::size_of::<NotifyIconDataW>() as u32;
    data.hwnd = hwnd;
    data.u_id = 1;
    Shell_NotifyIconW(NIM_DELETE, &data);
}

#[cfg(test)]
mod tests {
    use super::*;

    use Act::*;
    use WinState::*;
    // TrayEvent 不整体 use 进来：它的 Quit 会和 Act::Quit 撞名
    use TrayEvent::{Settings, Show, Toggle};

    /// 用户那张九宫格表，一格一行。
    ///
    /// 三个菜单项曾经全部在这里出过错，而且每次只在其中一两种窗口状态下发作：
    /// 「设置」在最小化时打不开、「退出」在眼前时变成了隐藏、在托盘里时没反应。
    /// 手点九次才碰上一次的 bug，写成表跑一遍才靠得住。
    #[test]
    fn the_tray_menu_does_the_same_thing_from_every_window_state() {
        let table = [
            // 「显示词典」：已经在眼前就别动，其余两种都得叫出来
            (Show, Front, Keep),
            (Show, Minimized, Summon { focus: true }),
            (Show, Hidden, Summon { focus: true }),
            // 「设置」：三种状态下都只开设置窗口，不把词典搬上屏幕。
            // 只有最小化要先收进托盘 —— 最小化的窗口不出帧，
            // 它的 immediate 子视口就画不出来。
            (Settings, Front, OpenSettings { tuck: false }),
            (Settings, Minimized, OpenSettings { tuck: true }),
            (Settings, Hidden, OpenSettings { tuck: false }),
            // 「退出」：三种状态下都是真退出，不是又藏一次
            (TrayEvent::Quit, Front, Act::Quit),
            (TrayEvent::Quit, Minimized, Act::Quit),
            (TrayEvent::Quit, Hidden, Act::Quit),
        ];
        for (ev, st, want) in table {
            assert_eq!(decide(ev, st), want, "{ev:?} 在 {st:?} 状态下");
        }
    }

    /// 左键单击是开关，和菜单里的「显示词典」不一样。
    #[test]
    fn a_left_click_only_hides_a_window_that_is_actually_on_screen() {
        assert_eq!(decide(Toggle, Front), Dismiss);
        // 最小化的窗口在 Win32 眼里仍然是 visible 的。把它当成「在眼前」
        // 去隐藏，表现就是点了没反应 —— 这正是之前的 bug。
        assert_eq!(decide(Toggle, Minimized), Summon { focus: true });
        assert_eq!(decide(Toggle, Hidden), Summon { focus: true });
    }

    /// 点「设置」只出设置窗口，不把词典搬上屏幕 —— 三种状态下都不是 `Summon`。
    ///
    /// 这条曾经是反的：为了让子视口有帧可用，三种状态都先把主窗口叫出来，
    /// 于是「点设置」变成了「词典 + 设置一起弹」。
    #[test]
    fn opening_settings_never_brings_the_dictionary_on_screen() {
        for st in [Front, Minimized, Hidden] {
            assert!(
                matches!(decide(Settings, st), OpenSettings { .. }),
                "{st:?} 状态下点设置不该把主窗口搬上屏幕"
            );
        }
    }

    /// 拿不到主窗口句柄时不能瞎猜成「藏起来了」——
    /// 那样左键单击会去「显示」一个本来就在眼前的窗口。
    #[test]
    fn a_missing_window_handle_is_treated_as_on_screen() {
        assert_eq!(win_state(0), Front);
    }

    #[test]
    fn default_hotkey_is_sane() {
        let h = Hotkey::default();
        assert!(h.is_valid());
        assert_eq!(h.label(), "Alt + 1");
    }

    #[test]
    fn a_hotkey_without_modifiers_is_rejected() {
        // 没有修饰键会把普通打字全截走
        assert!(!Hotkey { mods: 0, vk: b'D' as u32 }.is_valid());
        assert!(!Hotkey { mods: MOD_ALT, vk: 0 }.is_valid());
        assert!(Hotkey { mods: MOD_WIN, vk: 0x20 }.is_valid());
    }

    #[test]
    fn key_names_cover_what_the_picker_offers() {
        assert_eq!(vk_name(0x20), "Space");
        assert_eq!(vk_name(0x71), "F2");
        assert_eq!(vk_name(b'Z' as u32), "Z");
        assert_eq!(vk_name(b'5' as u32), "5");
    }

    #[test]
    fn label_lists_modifiers_in_a_stable_order() {
        let h = Hotkey { mods: MOD_WIN | MOD_SHIFT | MOD_CONTROL, vk: 0x20 };
        assert_eq!(h.label(), "Ctrl + Shift + Win + Space");
    }

    #[test]
    fn fallbacks_are_all_valid_and_distinct() {
        assert!(FALLBACKS.iter().all(|h| h.is_valid()));
        let mut seen = Vec::new();
        for h in FALLBACKS {
            assert!(!seen.contains(h), "备选里有重复：{}", h.label());
            seen.push(*h);
        }
        // 首选本身也该在备选列表里，这样「首选可用」时不会白试一轮
        assert!(FALLBACKS.contains(&Hotkey::default()));
    }

    #[test]
    fn the_default_avoids_the_combination_we_measured_as_taken() {
        // 本机实测 Ctrl+Alt+D 已被占用（RegisterHotKey 返回 1409）
        let taken = Hotkey { mods: MOD_CONTROL | MOD_ALT, vk: b'D' as u32 };
        assert_ne!(Hotkey::default(), taken);
    }

    #[test]
    fn hicon_is_actually_created() {
        // CreateBitmap 那条老路在这里会静默给出没有 alpha 的图标；
        // 这条断言至少保证句柄建得出来，不是空指针。
        // SAFETY: 只建一个图标再销毁，不涉及别的 Win32 状态。
        unsafe {
            for size in [16u32, 32] {
                let h = make_hicon(size);
                assert!(!h.is_null(), "{size}px 图标没建出来");
                destroy_icon(h);
            }
        }
    }

    /// NOTIFYICONDATAW 的 cbSize 必须等于真实结构大小，否则 shell 直接拒收。
    #[test]
    fn notify_icon_struct_matches_the_win32_layout() {
        // x64 上的 NOTIFYICONDATAW（含 guidItem 和 hBalloonIcon）是 976 字节
        assert_eq!(std::mem::size_of::<NotifyIconDataW>(), 976);
        assert_eq!(std::mem::size_of::<Guid>(), 16);
    }
}
