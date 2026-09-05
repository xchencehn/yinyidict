//! 两态面板：输入框下面同一块区域，要么候选、要么词条，不共存。
//!
//! 返回路径必须齐全（进了词条页要出得来）：
//! 打字退回候选 / ↑↓ 退回候选并保留原选中项 / Esc 两段（一退候选，二退清空）。
//!
//! eframe 0.36 的入口直接给一个满窗口的 `Ui`，所以这里不用 panel，自己切矩形 ——
//! 布局要的是精确控制，panel 的边距和背景反而要一层层拆掉。

use crate::fonts;
use crate::settings::{Settings, Theme};
use crate::shot;
use crate::theme::Palette;
use crate::tray::{Act, Tray};
use dict_core::{stabilize, Entry, Hit, Params, Store};
use egui::text::LayoutJob;
use egui::{Align, Color32, FontId, Layout, Pos2, Rect, Sense, TextFormat, UiBuilder, Vec2};
use std::time::Instant;

/// 词条出现时的淡入上浮时长。
/// 前进要交代「换内容了」，后退应该像撤销一样立刻发生 —— 所以只有进词条页有动效。
const SWAP_MS: f32 = 110.0;
/// 正文列宽上限。
const COL_W: f32 = 640.0;
const BAR_H: f32 = 40.0;
const NOTE_H: f32 = 26.0;

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    List,
    Entry,
}

pub struct App {
    store: Store,
    params: Params,
    palette: Palette,
    dark: bool,

    query: String,
    last_query: String,
    cands: Vec<Hit>,
    sel: usize,
    mode: Mode,
    entry: Option<Entry>,
    /// 当前词条的 id，用来问 store 这条义项是不是被重组过。
    entry_id: Option<u32>,
    entry_at: Instant,

    stable_order: bool,
    show_scores: bool,
    show_bar: bool,
    last_ms: f64,
    last_scanned: usize,
    focus_pending: bool,

    speech: Option<dict_tts::Tts>,
    note: String,
    font_report: String,

    cfg: Settings,
    cfg_path: std::path::PathBuf,
    cfg_dirty: bool,
    tray: Option<Tray>,
    visible: bool,
    /// 设置子窗口开着没有。
    settings_open: bool,
    /// 设置窗口刚打开，下一帧要把焦点给它。
    settings_focus_pending: bool,
    /// 第一帧画完没有。攒着的开机杂活要等它。
    painted: bool,
    /// 画过多少帧。只用来数「再等一帧」，溢出不可能。
    frames: u64,
    /// 攒着的「打开设置」：等 `frames` 到这个数才真的开。
    ///
    /// 设置是主窗口的 immediate 子视口，建它的时候父窗口必须已经在屏幕上。
    /// 托盘线程刚把窗口 `ShowWindow` 出来，这一轮 egui 循环里父窗口可能还没
    /// 画过一帧 —— eframe 建不出子窗口就跳过回调，egui 于是
    /// `panic: the user callback was never called`，**整个程序当场退出**。
    /// 所以一律等主窗口再画过一帧，代价是最多晚一帧，看不出来。
    settings_after: Option<u64>,
    /// 还没放出去的开机杂活，见 [`Deferred`]。
    deferred: Option<Deferred>,
    /// 正在等用户按下新的快捷键组合。
    capturing_hotkey: bool,
    /// 已经走了「退出」这条路。见 [`App::quit`]。
    quitting: bool,
    /// 诊断用的自截图脚本，只有设了 DICT_SHOT 才有。
    shot: Option<shot::Plan>,
}

/// 收到关闭请求时该怎么办。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CloseAct {
    /// 拦下来，改成收进托盘。
    ToTray,
    /// 放行，进程结束。
    Exit,
}

/// 关闭请求 → 动作。
///
/// `quitting` 这个入参不能省：托盘菜单的「退出」也是发 `Close` 实现的，
/// 不加区分就会被「关窗口 = 收进托盘」这条规则拦下来，点了只是又藏一次。
pub fn decide_close(quitting: bool, close_to_tray: bool, has_tray: bool) -> CloseAct {
    if !quitting && close_to_tray && has_tray {
        CloseAct::ToTray
    } else {
        CloseAct::Exit
    }
}

/// 窗口的出厂尺寸。窄而高 —— 词条是竖着长的，宽了只是浪费。
///
/// 真正用的是 `settings.json` 里记着的那个，这里只是没记录时的起点。
/// 设置子窗口用同一个尺寸：它是主窗口的子视口，两个不一样大的话来回切会跳。
pub const WINDOW_DEFAULT: [f32; 2] = [458.0, 632.0];
/// 再小就排不下词条页的两栏了。
pub const WINDOW_MIN: [f32; 2] = [400.0, 320.0];
/// 自绘标题栏的高度。
pub const TITLE_H: f32 = 34.0;

/// 开机就该做、但不该和开窗口抢资源的杂活。见 [`App::start_deferred`]。
pub struct Deferred {
    /// 语音引擎的配置。`None` = 这台机器上发音不可用。
    pub tts: Option<dict_tts::Config>,
    /// 词库目录，用来预热页缓存。
    pub index: std::path::PathBuf,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        store: Store,
        deferred: Deferred,
        mut note: String,
        cfg: Settings,
        cfg_path: std::path::PathBuf,
    ) -> Self {
        // 任务栏和标题栏图标：winit 的 with_icon 不总是够，直接给窗口发 WM_SETICON
        let mut main_hwnd = 0usize;
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            #[allow(unused_assignments)]
            match cc.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(w)) => {
                    main_hwnd = w.hwnd.get() as usize;
                    crate::tray::set_window_icon(main_hwnd);
                }
                _ => eprintln!("拿不到窗口句柄，任务栏图标只能靠 winit 那条路"),
            }
        }

        let shot = shot::Plan::from_env();
        if shot.is_some() {
            // 截图脚本要拍设置子窗口，但 eframe 不给 immediate 子视口做截图。
            // 内嵌模式把它画进主窗口，根视口的截图就能拍全 —— 仅诊断时如此。
            cc.egui_ctx.set_embed_viewports(true);
        }

        let report = fonts::install(&cc.egui_ctx);
        let dark = match cfg.theme {
            Theme::Auto => cc.egui_ctx.theme() == egui::Theme::Dark,
            Theme::Light => false,
            Theme::Dark => true,
        };
        let palette = Palette::of(dark);
        crate::theme::apply(&cc.egui_ctx, &palette, dark);

        // 托盘起不来不算致命 —— 词典本身照常能用，只是没有常驻和热键
        let mut cfg = cfg;
        let (tray, mut note) = match Tray::start(cc.egui_ctx.clone(), cfg.hotkey) {
            Ok(t) => {
                match t.active_hotkey() {
                    Some(hk) if hk != cfg.hotkey => {
                        note = format!(
                            "{} 被别的软件占了，唤出快捷键改成 {}",
                            cfg.hotkey.label(),
                            hk.label()
                        );
                        cfg.hotkey = hk;
                    }
                    Some(hk) => println!("托盘就绪 · 唤出 {}", hk.label()),
                    None => note = "所有候选快捷键都被占用，唤出快捷键未启用".into(),
                }
                (Some(t), note)
            }
            Err(e) => {
                eprintln!("托盘不可用：{e}");
                (None, format!("托盘不可用：{e}"))
            }
        };
        // 托盘线程要靠这个句柄读窗口状态、动窗口 —— 没有它托盘只能干瞪眼
        if let Some(t) = tray.as_ref() {
            t.set_main_hwnd(main_hwnd);
        }
        if cfg.start_hidden && tray.is_some() {
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        } else if cfg.start_hidden {
            note = "设置里要求启动时隐藏，但托盘不可用，仍然显示窗口".into();
        }

        let params = cfg.params();
        App {
            store,
            params,
            palette,
            dark,
            query: String::new(),
            last_query: String::new(),
            cands: Vec::new(),
            sel: 0,
            mode: Mode::List,
            entry: None,
            entry_id: None,
            entry_at: Instant::now(),
            stable_order: cfg.stable_order,
            show_scores: cfg.show_scores,
            show_bar: false,
            last_ms: 0.0,
            last_scanned: 0,
            focus_pending: true,
            speech: None,
            deferred: Some(deferred),
            painted: false,
            frames: 0,
            settings_after: None,
            font_report: report.join("  "),
            shot,
            visible: !(cfg.start_hidden && tray.is_some()),
            tray,
            cfg,
            cfg_path,
            cfg_dirty: false,
            settings_open: false,
            settings_focus_pending: false,
            capturing_hotkey: false,
            quitting: false,
            note,
        }
    }

    // ─────────────────────── 托盘与窗口 ───────────────────────

    /// 主窗口是不是「在眼前」。
    ///
    /// 收进托盘和最小化是**两种不同的隐身方式**：前者 `visible = false`，
    /// 后者窗口仍然 visible、只是缩进了任务栏。只看其中一个就会把另一种
    /// 状态判反 —— 托盘左键点最小化的窗口时会去「隐藏」它，等于没反应。
    fn open_settings(&mut self) {
        self.settings_open = true;
        self.settings_focus_pending = true;
    }

    /// 托盘已经把窗口显示出来了，这里把这个事实同步给 eframe。
    ///
    /// > **踩过的坑：不能只更新自己的 `visible`。** winit 自己缓存了一份窗口
    /// > 标志位，`set_visible` 是拿新旧标志**做差**再应用（`apply_diff`）。
    /// > 托盘线程那次 `ShowWindow` 绕过了 winit，它缓存里还记着「隐藏」——
    /// > 于是下一次 `Visible(false)` 会因为「和缓存里一样」被判定成无事发生，
    /// > 窗口再也收不进托盘。症状就是查完词点关闭没反应。
    /// > 所以每次都要顺着 eframe 再说一遍，把它的缓存拉回来对齐。
    fn revealed(&mut self, ctx: &egui::Context, focus: bool) {
        self.visible = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        if focus {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.focus_pending = true;
        }
        ctx.request_repaint();
    }

    /// 收进托盘。窗口的关闭按钮和托盘菜单都走这条。
    fn dismiss(&mut self, ctx: &egui::Context) {
        self.visible = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    /// 真正退出。
    ///
    /// `quitting` 这个标记不能省：关窗口默认是收进托盘，那条路会把
    /// `Close` 拦下来改成隐藏 —— 不加区分的话托盘菜单的「退出」发出去的
    /// `Close` 也会被拦住，点了只是又藏一次。
    fn quit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        self.save_settings();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// 把攒着的开机杂活放出去，第一帧画完才动手。
    ///
    /// 起窗口本身只要几十毫秒，慢的是陪着它一起启动的那些东西：语音引擎要
    /// 读 400 MB 模型加一份几十万行的词典，索引预热要读 60 MB。三样一起抢
    /// 磁盘，窗口就要等它们。挪到第一帧之后，这些代价落在用户已经能打字的
    /// 时间里 —— 总耗时没变，但等待没了。
    ///
    /// `start_hidden` 时不必等：本来就没有窗口要给人看。
    fn start_deferred(&mut self, ctx: &egui::Context) {
        if !self.painted && self.visible {
            return;
        }
        let Some(d) = self.deferred.take() else { return };
        // 上次退出时钉着的话，这次也钉上
        if self.cfg.pinned {
            self.push_pin(ctx);
        }
        // 语音先起：它是里面最慢的，而用户点播放键的时刻最早也在几秒之后
        self.speech = d.tts.map(dict_tts::Tts::start);
        crate::warm_index(&d.index);
    }

    /// 点窗口的关闭按钮 = 收进托盘；真要退出走托盘菜单的「退出」。
    ///
    /// 和托盘事件一样必须放在 `logic()` 里：窗口藏起来或最小化时 egui 不出帧，
    /// `ui()` 不会被调用 —— 搁在那里的话，「退出」在这两种状态下没人处理。
    fn pump_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        // 自截图脚本走完是要真退出的，别把它收进托盘
        let quitting = self.quitting || self.shot.is_some();
        if decide_close(quitting, self.cfg.close_to_tray, self.tray.is_some()) == CloseAct::ToTray {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dismiss(ctx);
        } else {
            self.save_settings();
        }
    }

    /// 处理托盘线程送来的事件。放在 `logic()` 里而不是 `ui()` —— 窗口藏起来时
    /// egui 不出帧，`ui()` 根本不会被调用，热键就没人响应了。
    fn pump_tray(&mut self, ctx: &egui::Context) {
        // 攒着的「打开设置」：主窗口又画过一帧，说明叫窗口的命令已经生效了
        if let Some(n) = self.settings_after {
            if self.frames >= n {
                self.settings_after = None;
                self.open_settings();
            } else {
                ctx.request_repaint();
            }
        }
        // 窗口的显隐已经由托盘线程当场做掉了（见 tray::apply），这里做两件事：
        // 更新自己的记账，**并且把同样的状态再通过 eframe 说一遍**。
        while let Some(act) = self.tray.as_ref().and_then(|t| t.try_recv()) {
            match act {
                Act::Keep => {}
                Act::Summon { focus } => self.revealed(ctx, focus),
                Act::Dismiss => self.dismiss(ctx),
                Act::OpenSettings => {
                    self.revealed(ctx, false);
                    // 见 settings_after 的文档：早一帧开会 panic
                    self.settings_after = Some(self.frames + 1);
                }
                Act::Quit => self.quit(ctx),
            }
        }
    }

    fn save_settings(&mut self) {
        if !self.cfg_dirty {
            return;
        }
        self.cfg.apply_params(&self.params);
        self.cfg.stable_order = self.stable_order;
        self.cfg.show_scores = self.show_scores;
        self.cfg.save(&self.cfg_path);
        self.cfg_dirty = false;
    }

    /// 把语速 / 载体句开关的改动送到 TTS 线程。
    fn push_tuning(&self) {
        if let Some(t) = self.speech.as_ref() {
            t.tune(self.cfg.speed, self.cfg.carrier);
        }
    }

    // ─────────────────────── 检索 ───────────────────────

    fn refresh(&mut self) {
        let t = Instant::now();
        let r = self.store.search(&self.query, &self.params);
        self.last_ms = t.elapsed().as_secs_f64() * 1000.0;
        self.last_scanned = r.scanned;

        // 稳定排序：query 是上一次的延长时只过滤不重排。
        // 用户眼睛已经锁定第 3 项时它跳到第 1 项，比慢更烦人。
        let extends = !self.last_query.is_empty()
            && self.query.len() > self.last_query.len()
            && self.query.to_lowercase().starts_with(&self.last_query.to_lowercase());
        self.cands =
            if self.stable_order && extends { stabilize(&self.cands, r.hits) } else { r.hits };
        self.last_query = self.query.clone();
        self.sel = 0;
        self.mode = Mode::List;
    }

    fn open(&mut self, i: usize) {
        let Some(h) = self.cands.get(i) else { return };
        let id = h.id;
        match self.store.entry(id) {
            Ok(e) => self.show(id, e),
            Err(err) => self.note = format!("读取词条失败: {err}"),
        }
    }

    fn open_word(&mut self, word: &str) {
        let Some(id) = self.store.lookup_word(word) else { return };
        if let Ok(e) = self.store.entry(id) {
            self.show(id, e);
        }
    }

    fn show(&mut self, id: u32, e: Entry) {
        // 本机 RTF 约 0.35，一个词头要近一秒。开页就先把它算好，
        // 等用户真去点播放键时就是零等待。
        if let Some(t) = self.speech.as_ref() {
            t.prefetch_word(&e.word, e.is_zh());
        }
        self.entry = Some(e);
        self.entry_id = Some(id);
        self.entry_at = Instant::now();
        self.mode = Mode::Entry;
    }

    // ─────────────────────── 键盘 ───────────────────────

    /// 在 TextEdit 拿到事件之前先把导航键截走。
    fn handle_keys(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let (down, up, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });

        if down || up {
            // 从词条页按方向键 = 回到候选并保留原选中项，可以接着翻下一个
            if self.mode == Mode::Entry {
                self.mode = Mode::List;
            } else if !self.cands.is_empty() {
                let d: isize = if down { 1 } else { -1 };
                let n = self.cands.len() as isize;
                self.sel = (self.sel as isize + d).clamp(0, n - 1) as usize;
            }
        }
        if enter {
            // 回车 = 打开当前选中的候选，不是拿原始输入去精确查询 ——
            // 否则 `zhonggu` 回车会查无结果。
            self.open(self.sel);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F1)) {
            self.show_bar = !self.show_bar;
        }
        if esc {
            match self.mode {
                Mode::Entry => self.mode = Mode::List,
                Mode::List => {
                    self.query.clear();
                    self.last_query.clear();
                    self.cands.clear();
                    self.sel = 0;
                    self.last_ms = 0.0;
                }
            }
        }
    }

    // ─────────────────────── 文本构件 ───────────────────────

    fn text(&self, s: &str, font: FontId, color: Color32) -> LayoutJob {
        let mut job = LayoutJob::default();
        job.append(s, 0.0, TextFormat { font_id: font, color, ..Default::default() });
        job
    }

    /// 带调拼音，每个音节按声调着色。全局只有这一处用色。
    fn pinyin_job(&self, e: &Entry, size: f32, gap: f32) -> LayoutJob {
        let mut job = LayoutJob::default();
        let f = fonts::sans(size);
        for (i, syl) in e.reading.split_whitespace().enumerate() {
            let color = self.palette.tone_of(e.tones.get(i).copied().unwrap_or(0));
            job.append(
                syl,
                if i == 0 { 0.0 } else { gap },
                TextFormat { font_id: f.clone(), color, ..Default::default() },
            );
        }
        job
    }

    // ─────────────────────── 候选 ───────────────────────

    fn candidates(&mut self, ui: &mut egui::Ui) {
        if self.cands.is_empty() {
            if !self.query.trim().is_empty() {
                ui.add_space(10.0);
                ui.label(self.text("没有匹配", fonts::sans(14.0), self.palette.faint));
            }
            return;
        }

        let p = self.palette;
        let mut clicked: Option<usize> = None;
        let rows: Vec<(usize, Hit)> = self.cands.iter().copied().enumerate().collect();

        for (i, h) in rows {
            let Ok(e) = self.store.entry(h.id) else { continue };
            let w = ui.available_width();
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 30.0), Sense::click());
            if i == self.sel {
                ui.painter().rect_filled(rect, 3.0, p.hit);
            }
            if resp.clicked() {
                clicked = Some(i);
            }

            let inner = Rect::from_min_max(
                rect.min + Vec2::new(10.0, 0.0),
                rect.max - Vec2::new(10.0, 0.0),
            );
            ui.scope_builder(
                UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 13.0;

                    // 词形：中文衬线，英文半粗无衬线 —— 字体本身就是类型信号
                    let font = if e.is_zh() { fonts::serif(19.0) } else { fonts::semibold(17.0) };
                    ui.allocate_ui_with_layout(
                        Vec2::new(102.0, 26.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.add(egui::Label::new(self.text(&e.word, font, p.ink)).truncate());
                        },
                    );

                    // 注音：中文着色，英文灰 IPA
                    ui.allocate_ui_with_layout(
                        Vec2::new(128.0, 26.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            let job = if e.is_zh() {
                                self.pinyin_job(&e, 13.5, 3.0)
                            } else {
                                self.text(&e.reading, fonts::sans(13.0), p.muted)
                            };
                            ui.add(egui::Label::new(job).truncate());
                        },
                    );

                    if self.show_scores {
                        ui.allocate_ui_with_layout(
                            Vec2::new(92.0, 26.0),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.label(self.text(h.lane.label(), fonts::sans(10.5), p.faint));
                                ui.label(self.text(
                                    &format!("{:.2}", h.score),
                                    fonts::sans(11.0),
                                    p.faint,
                                ));
                            },
                        );
                    }

                    let gloss: Vec<&str> =
                        e.senses.iter().take(3).map(|s| s.text.as_str()).collect();
                    let sep = if e.is_zh() { "; " } else { "；" };
                    ui.add(
                        egui::Label::new(self.text(&gloss.join(sep), fonts::sans(13.5), p.muted))
                            .truncate(),
                    );
                },
            );
        }

        if let Some(i) = clicked {
            self.sel = i;
            self.open(i);
        }
    }

    /// 发声按钮：一个点，右边两道向外扩的弧 —— 横过来的信号图标。
    ///
    /// 原来用的是文字 `▶`。三角形是「播放」，播放的是一段录音；
    /// 这里按下去是**现场合成**这个词的读音，画声波比画播放键切题，
    /// 而且字形按钮的大小受字体摆布，画出来的能压到 13 px 还不糊。
    ///
    /// 弧线用折线近似：这个尺寸下再多的点也只落在同一批像素上。
    fn sound_icon(&self, ui: &mut egui::Ui, size: f32, tip: &str) -> bool {
        let p = self.palette;
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::click());
        let color = if resp.hovered() { p.ink_soft } else { p.faint };
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let painter = ui.painter();
        // 声源在左侧三分之一处，弧从它往右扩 —— 整体重心才在图标中间
        let c = egui::pos2(rect.left() + size * 0.30, rect.center().y);
        painter.circle_filled(c, size * 0.105, color);

        let w = (size * 0.085).max(1.0);
        for k in [0.30f32, 0.50] {
            let r = size * k;
            // ±48°，开口朝右
            let pts: Vec<egui::Pos2> = (0..=8)
                .map(|i| {
                    let a = (-48.0 + 96.0 * i as f32 / 8.0).to_radians();
                    egui::pos2(c.x + r * a.cos(), c.y + r * a.sin())
                })
                .collect();
            painter.add(egui::Shape::line(pts, egui::Stroke::new(w, color)));
        }

        resp.on_hover_text(tip).clicked()
    }

    /// 自绘标题栏。
    ///
    /// 为什么不用系统的：一是那条灰边在这套「无卡片无边框」的配色里格格不入；
    /// 二是**原生标题栏加不了按钮** —— 想要一颗「钉在最前」的钉子，只能自己画。
    ///
    /// 代价是拖动、双击最大化、八个方向的缩放都得自己接回来，见 `frame_drag`。
    /// 好在 winit 底下走的还是 `WM_NCLBUTTONDOWN`，贴边分屏这些照常有。
    fn title_bar(&mut self, ui: &mut egui::Ui, rect: Rect, main: bool) {
        let p = self.palette;
        let ctx = ui.ctx().clone();
        ui.painter().rect_filled(rect, 0.0, p.ground);
        // 一条极淡的分隔线，只为把标题栏和内容分开，不做边框
        ui.painter().hline(rect.x_range(), rect.max.y - 0.5, egui::Stroke::new(1.0, p.rule));

        let title = if main { "词典" } else { "设置" };
        ui.painter().text(
            rect.left_center() + Vec2::new(14.0, 0.0),
            egui::Align2::LEFT_CENTER,
            title,
            fonts::sans(12.5),
            p.faint,
        );

        // 按钮从右往左排
        const BW: f32 = 40.0;
        let mut x = rect.max.x;
        let mut slot = |n: usize| {
            let r =
                Rect::from_min_max(Pos2::new(x - BW, rect.min.y), Pos2::new(x, rect.max.y - 1.0));
            x -= BW;
            let _ = n;
            r
        };

        let close_r = slot(0);
        let (max_r, min_r, pin_r) = if main {
            (slot(1), slot(2), slot(3))
        } else {
            // 设置窗口只留关闭：最大化没意义，最小化会把父窗口一起带走
            (Rect::NOTHING, Rect::NOTHING, Rect::NOTHING)
        };

        // ── 关闭 ──
        if self.title_btn(ui, close_r, Glyph::Close, false) {
            if main {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.settings_open = false;
                self.save_settings();
            }
        }
        if !main {
            self.frame_drag(ui, rect, x);
            return;
        }

        // ── 最大化 / 还原 ──
        let maxed = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if self.title_btn(ui, max_r, Glyph::Max { restore: maxed }, false) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maxed));
        }

        // ── 最小化 ──
        if self.title_btn(ui, min_r, Glyph::Min, false) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }

        // ── 钉在最前 ──
        if self.title_btn(ui, pin_r, Glyph::Pin, self.cfg.pinned) {
            self.cfg.pinned = !self.cfg.pinned;
            self.cfg_dirty = true;
            self.push_pin(&ctx);
        }

        self.frame_drag(ui, rect, x);
    }

    /// 标题栏上的一个按钮。`on` = 处于按下状态（只有钉子会用到）。
    fn title_btn(&self, ui: &mut egui::Ui, r: Rect, g: Glyph, on: bool) -> bool {
        if r == Rect::NOTHING {
            return false;
        }
        let p = self.palette;
        let resp = ui.interact(r, ui.id().with(("titlebtn", g.id())), egui::Sense::click());
        let hot = resp.hovered();
        if hot {
            // 关闭键的悬停底色单独给红，别的都用中性色 —— 这是全世界的习惯，
            // 手指还没落下去就该知道自己指的是哪个
            let bg = if matches!(g, Glyph::Close) { p.tone[1] } else { p.hit };
            ui.painter().rect_filled(r, 0.0, bg);
        }
        let ink = match (g, hot) {
            (Glyph::Close, true) => p.ground,
            _ if on => p.tone[1],
            (_, true) => p.ink,
            _ => p.muted,
        };
        g.paint(ui.painter(), r.center(), ink, on);
        resp.clicked()
    }

    /// 标题栏空白处拖动 + 双击最大化。
    fn frame_drag(&self, ui: &mut egui::Ui, bar: Rect, buttons_left: f32) {
        let ctx = ui.ctx().clone();
        let drag_area = Rect::from_min_max(bar.min, Pos2::new(buttons_left, bar.max.y));
        let resp = ui.interact(drag_area, ui.id().with("titledrag"), egui::Sense::click_and_drag());
        if resp.double_clicked() {
            let maxed = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maxed));
        } else if resp.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
    }

    /// 窗口八个方向的缩放边。自绘标题栏把系统那圈边框丢了，得自己接回来。
    ///
    /// **必须在内容之后注册**：egui 的命中判定是后来者居上，先注册的话
    /// 输入框、滚动区会把贴边那几像素的点击全吃掉，边就永远拖不动。
    fn resize_edges(&self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // 最大化时没有缩放边可言
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            return;
        }
        use egui::{CursorIcon as C, ResizeDirection as D};
        const E: f32 = 6.0;
        let w = ui.max_rect();
        // 先角后边：角落要压在边上面，否则永远拖不到斜角
        let corners = [
            (Rect::from_min_size(w.min, Vec2::splat(E)), D::NorthWest, C::ResizeNwSe),
            (
                Rect::from_min_size(Pos2::new(w.max.x - E, w.min.y), Vec2::splat(E)),
                D::NorthEast,
                C::ResizeNeSw,
            ),
            (
                Rect::from_min_size(Pos2::new(w.min.x, w.max.y - E), Vec2::splat(E)),
                D::SouthWest,
                C::ResizeNeSw,
            ),
            (
                Rect::from_min_size(w.max - Vec2::splat(E), Vec2::splat(E)),
                D::SouthEast,
                C::ResizeNwSe,
            ),
        ];
        let edges = [
            (
                Rect::from_min_max(w.min, Pos2::new(w.max.x, w.min.y + E)),
                D::North,
                C::ResizeVertical,
            ),
            (
                Rect::from_min_max(Pos2::new(w.min.x, w.max.y - E), w.max),
                D::South,
                C::ResizeVertical,
            ),
            (
                Rect::from_min_max(w.min, Pos2::new(w.min.x + E, w.max.y)),
                D::West,
                C::ResizeHorizontal,
            ),
            (
                Rect::from_min_max(Pos2::new(w.max.x - E, w.min.y), w.max),
                D::East,
                C::ResizeHorizontal,
            ),
        ];
        for (i, (r, dir, cur)) in corners.iter().chain(edges.iter()).enumerate() {
            let resp = ui.interact(*r, ui.id().with(("resize", i)), egui::Sense::drag());
            if resp.hovered() || resp.dragged() {
                ctx.set_cursor_icon(*cur);
            }
            if resp.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(*dir));
            }
        }
    }

    /// 窗口被拖动改过大小就记下来，下次启动照这个开。
    ///
    /// 最大化时不记 —— 记了的话点「还原」会还原成全屏尺寸，等于还原不回去。
    /// 只在差出一个点以上时才落盘，免得每帧都把设置标成脏的。
    fn remember_size(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            return;
        }
        let Some(r) = ctx.input(|i| i.viewport().inner_rect) else { return };
        let now = [r.width(), r.height()];
        if now[0] < WINDOW_MIN[0] || now[1] < WINDOW_MIN[1] {
            return; // 最小化的那一瞬间会报出 0×0
        }
        if (now[0] - self.cfg.window[0]).abs() > 1.0 || (now[1] - self.cfg.window[1]).abs() > 1.0 {
            self.cfg.window = now;
            self.cfg_dirty = true;
        }
    }

    /// 把「钉在最前」同步给窗口系统。
    fn push_pin(&self, ctx: &egui::Context) {
        let level = if self.cfg.pinned {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
    }

    // ─────────────────────── 词条页 ───────────────────────

    fn entry_page(&mut self, ui: &mut egui::Ui) {
        let Some(e) = self.entry.clone() else { return };
        let p = self.palette;
        let zh = e.is_zh();
        let mut goto: Option<String> = None;
        let mut say: Option<(String, bool)> = None;

        ui.add_space(18.0);

        // ── 词头行 ──
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            let font = if zh { fonts::serif(50.0) } else { fonts::semibold(44.0) };
            ui.label(self.text(&e.word, font, p.ink));
            if zh {
                ui.label(self.pinyin_job(&e, 20.0, 6.0));
            } else if !e.reading.is_empty() {
                ui.label(self.text(&e.reading, fonts::sans(18.0), p.muted));
            }
            if self.sound_icon(ui, 17.0, "朗读词头") {
                say = Some((e.word.clone(), zh));
            }
        });

        // ── 元信息 ──
        //
        // 只留繁体。词频和考试标签（四级/六级/考研/托福/GRE、柯林斯星级）是语料
        // 统计，不是这个词本身的信息 —— 查词的人要的是词义，不是它的应试来历。
        // 词频仍然在幕后决定候选排序，只是不占版面。
        let mut meta: Vec<String> = Vec::new();
        if !e.trad.is_empty() {
            meta.push(format!("繁体 {}", e.trad));
        }
        if !meta.is_empty() {
            ui.add_space(9.0);
            ui.label(self.text(&meta.join("　·　"), fonts::sans(13.0), p.faint));
        }
        if !e.forms.is_empty() {
            ui.add_space(6.0);
            ui.label(self.text(&e.forms, fonts::sans(13.0), p.muted));
        }

        // ── 义项 ──
        let numbered = e.senses.len() > 1;
        for (i, s) in e.senses.iter().enumerate() {
            ui.add_space(15.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(
                    Vec2::new(26.0, 20.0),
                    Layout::right_to_left(Align::Min),
                    |ui| {
                        if numbered {
                            ui.label(self.text(&format!("{}", i + 1), fonts::sans(13.0), p.faint));
                        }
                    },
                );
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let pos = if s.pos.is_empty() { &e.pos } else { &s.pos };
                        if !pos.is_empty() {
                            ui.label(self.text(pos, fonts::serif(13.0), p.muted));
                        }
                        // 中文词条的释义是英文、英文词条的释义是中文 —— 字体跟着内容走
                        let f = if zh { fonts::sans(16.5) } else { fonts::serif(17.5) };
                        ui.label(self.text(&s.text, f, p.ink));
                    });
                    if !s.note.is_empty() {
                        ui.add_space(3.0);
                        ui.label(self.text(&s.note, fonts::sans(14.0), p.ink_soft));
                    }
                    if !s.reg.is_empty() {
                        ui.add_space(4.0);
                        ui.label(self.text(&s.reg, fonts::sans(12.0), p.muted));
                    }
                });
            });
        }

        // ── 例句 ──
        // 例句音频比词头更能体现质量差距：有完整上下文，韵律天然更好。
        if !e.examples.is_empty() {
            ui.add_space(6.0);
        }
        for ex in &e.examples {
            ui.add_space(11.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(40.0);
                let bar_x = ui.cursor().min.x;
                let top = ui.cursor().min.y;
                ui.add_space(13.0);
                ui.vertical(|ui| {
                    /// 例句发声键的边长。比词头那个小一圈 —— 例句是次要内容。
                    const ICON: f32 = 12.5;
                    let fa = if zh { fonts::serif(15.5) } else { fonts::sans(15.5) };
                    let fb = if zh { fonts::sans(13.5) } else { fonts::serif(14.5) };
                    // 两行各带一个发声键：例句是中英对照的，查中文时想听英文
                    // 那句、查英文时想听中文那句，都是常事
                    for (text, font, color, is_zh) in
                        [(&ex.a, fa, p.ink_soft, zh), (&ex.b, fb, p.muted, !zh)]
                    {
                        // 用 wrapped：长句子换行后，发声键跟在最后一行末尾，
                        // 不会被挤出可视区
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let size = font.size;
                            ui.label(self.text(text, font, color));
                            ui.add_space(7.0);
                            // 图标比字矮一圈，按行高压下去才坐在同一条中线上，
                            // 而不是顶着行首浮着
                            let hit = ui.vertical(|ui| {
                                ui.add_space(((size * 1.32 - ICON) / 2.0).max(0.0));
                                self.sound_icon(ui, ICON, "朗读这一句")
                            });
                            if hit.inner {
                                say = Some((text.to_string(), is_zh));
                            }
                        });
                        ui.add_space(2.0);
                    }
                    let bottom = ui.cursor().min.y;
                    ui.painter().vline(bar_x, top..=bottom - 4.0, egui::Stroke::new(2.0, p.rule));
                });
            });
        }

        // ── 对应词条 ──
        let links: Vec<String> =
            e.xrefs.iter().filter(|w| self.store.lookup_word(w).is_some()).cloned().collect();
        if !links.is_empty() {
            ui.add_space(22.0);
            let y = ui.cursor().min.y;
            ui.painter().hline(
                ui.cursor().min.x..=ui.cursor().min.x + ui.available_width(),
                y,
                egui::Stroke::new(1.0, p.rule),
            );
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(self.text("对应词条", fonts::sans(12.5), p.faint));
                for w in &links {
                    let f = if w.chars().any(dict_core::is_cjk) {
                        fonts::serif(14.0)
                    } else {
                        fonts::sans(14.0)
                    };
                    let b = egui::Button::new(self.text(w, f, p.ink_soft))
                        .fill(Color32::TRANSPARENT)
                        .stroke(egui::Stroke::new(1.0, p.rule));
                    if ui.add(b).clicked() {
                        goto = Some(w.clone());
                    }
                }
            });
        }

        // 原始释义不在这里显示。中文侧的义项被重组过，核对的能力没有丢 ——
        // `raw` 字段仍在词库里，`dict-query --raw` 一条命令就能调出原文对照。
        ui.add_space(40.0);

        if let Some(w) = goto {
            self.open_word(&w);
        }
        if let Some((t, zh)) = say {
            self.speak(&t, zh);
        }
    }

    fn speak(&mut self, text: &str, zh: bool) {
        let Some(tts) = self.speech.as_ref() else {
            self.note = "发音不可用".into();
            return;
        };
        match tts.status() {
            dict_tts::Status::Failed(e) => self.note = format!("语音模型加载失败: {e}"),
            dict_tts::Status::Loading => self.note = "语音模型还在加载".into(),
            dict_tts::Status::Ready { .. } => {
                // 单个词走载体句合成再裁剪，整句直接合成
                let r = if text.chars().count() <= 8 && !text.contains(['。', '.', '，', ',']) {
                    tts.say_word(text, zh)
                } else {
                    tts.say_sentence(text)
                };
                if let Err(e) = r {
                    self.note = format!("合成失败: {e}");
                }
            }
        }
    }

    // ─────────────────────── 布局 ───────────────────────

    fn column(&mut self, ui: &mut egui::Ui) {
        let p = self.palette;
        let avail = ui.available_width();
        let w = COL_W.min(avail - 48.0).max(240.0);
        let pad = ((avail - w) / 2.0).max(0.0);

        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(pad);
            ui.vertical(|ui| {
                ui.set_width(w);
                ui.add_space(56.0);

                // 提示文字要先算好：TextEdit 借走了 self.query，之后就不能再借 self。
                // 固定两个字母 —— 空框里放长句子会被当成已经输进去的内容，
                // 而这个框唯一需要说明的就是「中英都能查」。
                let hint = self.text("en-zh", fonts::sans(30.0), p.ghost);
                let te = egui::TextEdit::singleline(&mut self.query)
                    .font(fonts::sans(30.0))
                    .desired_width(w)
                    .frame(egui::Frame::NONE)
                    .hint_text(hint);
                let resp = ui.add(te);
                // 输入框默认持有焦点：打字随时都该回到查词。
                // 但只在没有别的控件被聚焦时才抢 —— 否则拖 F1 调参条的滑块时
                // 焦点每帧被夺走，滑块根本拖不动。
                let someone_else_focused =
                    ui.ctx().memory(|m| m.focused()).is_some_and(|id| id != resp.id);
                // 设置窗口开着时别抢焦点，否则那边的快捷键捕获永远录不到按键
                let want_focus = !self.settings_open;
                if want_focus
                    && (self.focus_pending || (!resp.has_focus() && !someone_else_focused))
                {
                    resp.request_focus();
                    self.focus_pending = false;
                }
                let typed = resp.changed();

                ui.add_space(10.0);
                let x0 = ui.cursor().min.x;
                let y = ui.cursor().min.y;
                ui.painter().hline(
                    x0..=x0 + w,
                    y,
                    egui::Stroke::new(1.0, if resp.has_focus() { p.ink_soft } else { p.rule }),
                );
                ui.add_space(7.0);

                if typed {
                    self.refresh();
                }

                match self.mode {
                    Mode::List => {
                        egui::ScrollArea::vertical()
                            .id_salt("list")
                            .auto_shrink([false, false])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .show(ui, |ui| self.candidates(ui));
                    }
                    Mode::Entry => {
                        let t = (self.entry_at.elapsed().as_secs_f32() * 1000.0 / SWAP_MS)
                            .clamp(0.0, 1.0);
                        if t < 1.0 {
                            ui.ctx().request_repaint();
                        }
                        let eased = 1.0 - (1.0 - t) * (1.0 - t);
                        ui.add_space(5.0 * (1.0 - eased));
                        ui.scope(|ui| {
                            ui.set_opacity(eased);
                            egui::ScrollArea::vertical()
                                .id_salt("entry")
                                .auto_shrink([false, false])
                                .scroll_bar_visibility(
                                    egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                                )
                                .show(ui, |ui| self.entry_page(ui));
                        });
                    }
                }
            });
        });
    }

    fn bar(&mut self, ui: &mut egui::Ui) {
        let p = self.palette;
        let r = ui.max_rect();
        ui.painter().hline(r.left()..=r.right(), r.top(), egui::Stroke::new(1.0, p.rule));
        let inner = r.shrink2(Vec2::new(24.0, 10.0));
        let before = self.params;

        // 状态文字直接用 painter 右对齐画。放进布局里会和左边的控件抢空间，
        // 宽度不够时就叠在「配色」按钮上。
        let status = if self.last_ms > 0.0 {
            format!("{:.2} ms · {} 候选", self.last_ms, self.cands.len())
        } else {
            String::from("—")
        };
        let status_w = ui
            .painter()
            .text(
                inner.right_center(),
                egui::Align2::RIGHT_CENTER,
                &status,
                fonts::sans(12.0),
                p.faint,
            )
            .width();

        let controls = Rect::from_min_max(
            inner.min,
            Pos2::new((inner.max.x - status_w - 20.0).max(inner.min.x), inner.max.y),
        );
        ui.scope_builder(
            UiBuilder::new().max_rect(controls).layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.style_mut().override_font_id = Some(fonts::sans(12.0));
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.spacing_mut().slider_width = 44.0;
                ui.set_max_width(controls.width());
                ui.checkbox(&mut self.show_scores, "打分");
                ui.checkbox(&mut self.stable_order, "稳定排序");
                ui.add(
                    egui::Slider::new(&mut self.params.exact_bonus, 0.0..=12.0)
                        .text("exact")
                        .fixed_decimals(1),
                );
                ui.add(
                    egui::Slider::new(&mut self.params.lambda, 0.0..=1.2)
                        .text("λ")
                        .fixed_decimals(2),
                );
                ui.add(
                    egui::Slider::new(&mut self.params.secondary, 0.0..=1.0)
                        .text("释义路")
                        .fixed_decimals(2),
                );
                if ui.button("配色").clicked() {
                    self.dark = !self.dark;
                    self.palette = Palette::of(self.dark);
                    let ctx = ui.ctx().clone();
                    crate::theme::apply(&ctx, &self.palette, self.dark);
                }
            },
        );

        // 调参滑块动过就必须重排，否则界面和参数对不上
        if !params_eq(&before, &self.params) {
            self.cfg_dirty = true;
            self.last_query.clear(); // 参数变了，重排是应该的，别再套稳定排序
            self.refresh();
        }
    }

    // ─────────────────────── 设置页 ───────────────────────

    /// 一行设置：左边固定宽的标签，右边控件。整页对齐靠它，不靠每处手调间距。
    fn row(&self, ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
        let p = self.palette;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.allocate_ui_with_layout(
                Vec2::new(112.0, 24.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(self.text(label, fonts::sans(13.5), p.muted));
                },
            );
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 6.0;
            add(ui);
        });
        ui.add_space(9.0);
    }

    fn section(&self, ui: &mut egui::Ui, title: &str) {
        ui.add_space(20.0);
        ui.label(self.text(title, fonts::sans(12.0), self.palette.faint));
        ui.add_space(4.0);
        let x = ui.cursor().min.x;
        let y = ui.cursor().min.y;
        ui.painter().hline(
            x..=x + ui.available_width(),
            y,
            egui::Stroke::new(1.0, self.palette.rule),
        );
        ui.add_space(11.0);
    }

    /// 一行快捷键：显示当前组合 + 「重设」进入捕获态。
    fn hotkey_row(&mut self, ui: &mut egui::Ui, label: &str) {
        let p = self.palette;
        let capturing = self.capturing_hotkey;
        let cur = self.cfg.hotkey;
        let taken = self.tray.as_ref().is_some_and(|t| t.active_hotkey().is_none());
        let no_tray = self.tray.is_none();
        let text = if capturing { String::from("按下新的组合…") } else { cur.label() };
        let mut toggle = false;

        self.row(ui, label, |ui| {
            let color = if capturing { p.tone[1] } else { p.ink };
            ui.label(self.text(&text, fonts::sans(14.0), color));
            ui.add_space(10.0);
            if ui.button(if capturing { "取消" } else { "重设" }).clicked() {
                toggle = true;
            }
            if no_tray {
                ui.add_space(8.0);
                ui.label(self.text("托盘不可用，未注册", fonts::sans(12.0), p.faint));
            } else if taken {
                ui.add_space(8.0);
                ui.label(self.text("被别的软件占用", fonts::sans(12.0), p.tone[1]));
            }
        });
        if toggle {
            self.capturing_hotkey = !capturing;
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        let p = self.palette;
        ui.add_space(14.0);
        ui.label(self.text("设置", fonts::serif(26.0), p.ink));
        ui.add_space(3.0);
        ui.label(self.text("Esc 返回查词", fonts::sans(12.0), p.faint));

        // ── 发音 ──
        self.section(ui, "发音");
        let status = self.speech.as_ref().map(|t| t.status());
        match status {
            Some(dict_tts::Status::Ready { sample_rate, model }) => {
                self.row(ui, "模型", |ui| {
                    ui.label(self.text(
                        &format!("{model} · {sample_rate} Hz"),
                        fonts::sans(13.0),
                        p.ink_soft,
                    ));
                });

                let mut speed = self.cfg.speed;
                self.row(ui, "语速", |ui| {
                    ui.add(egui::Slider::new(&mut speed, 0.5..=2.0).fixed_decimals(2));
                });
                if (speed - self.cfg.speed).abs() > f32::EPSILON {
                    self.cfg.speed = speed;
                    self.cfg_dirty = true;
                    self.push_tuning();
                }

                let mut carrier = self.cfg.carrier;
                self.row(ui, "载体句裁剪", |ui| {
                    ui.checkbox(&mut carrier, "");
                    ui.label(self.text(
                        "合成「这个词读作，X」再切出 X；关掉即直接喂单词",
                        fonts::sans(12.0),
                        p.faint,
                    ));
                });
                if carrier != self.cfg.carrier {
                    self.cfg.carrier = carrier;
                    self.cfg_dirty = true;
                    self.push_tuning();
                }
            }
            Some(dict_tts::Status::Loading) => self.row(ui, "模型", |ui| {
                ui.label(self.text("加载中…", fonts::sans(13.0), p.muted));
            }),
            Some(dict_tts::Status::Failed(e)) => self.row(ui, "模型", |ui| {
                ui.label(self.text(&format!("加载失败：{e}"), fonts::sans(13.0), p.muted));
            }),
            None => self.row(ui, "模型", |ui| {
                ui.label(self.text("不可用", fonts::sans(13.0), p.muted));
            }),
        }

        // ── 窗口 ──
        self.section(ui, "窗口");
        self.hotkey_row(ui, "唤出词典");

        let (mut to_tray, mut hidden) = (self.cfg.close_to_tray, self.cfg.start_hidden);
        self.row(ui, "关闭窗口时", |ui| {
            ui.checkbox(&mut to_tray, "");
            ui.label(self.text("收进托盘而不是退出", fonts::sans(13.0), p.ink_soft));
        });
        self.row(ui, "启动时", |ui| {
            ui.checkbox(&mut hidden, "");
            ui.label(self.text("只驻留托盘，不显示窗口", fonts::sans(13.0), p.ink_soft));
        });

        // 开机自启的状态每帧现读注册表 —— 用户可能从任务管理器的「启动」页
        // 把它关掉，我们自己记一份的话这里就会显示一个骗人的勾。
        // 见 autostart 模块开头。
        let mut auto = crate::autostart::enabled();
        let was = auto;
        self.row(ui, "开机自启", |ui| {
            ui.checkbox(&mut auto, "");
            ui.label(self.text("登录时自动运行", fonts::sans(13.0), p.ink_soft));
        });
        if auto != was && !crate::autostart::set(auto) {
            self.note = "改不了开机自启（注册表写不进去）".into();
        }
        if to_tray != self.cfg.close_to_tray || hidden != self.cfg.start_hidden {
            self.cfg.close_to_tray = to_tray;
            self.cfg.start_hidden = hidden;
            self.cfg_dirty = true;
        }

        let cur_theme = self.cfg.theme;
        let mut pick = cur_theme;
        self.row(ui, "配色", |ui| {
            for t in [Theme::Auto, Theme::Light, Theme::Dark] {
                if ui.selectable_label(cur_theme == t, t.label()).clicked() {
                    pick = t;
                }
            }
        });
        if pick != cur_theme {
            self.cfg.theme = pick;
            self.cfg_dirty = true;
            let ctx = ui.ctx().clone();
            self.dark = match pick {
                Theme::Auto => ctx.theme() == egui::Theme::Dark,
                Theme::Light => false,
                Theme::Dark => true,
            };
            self.palette = Palette::of(self.dark);
            crate::theme::apply(&ctx, &self.palette, self.dark);
        }

        // ── 词库 ──
        self.section(ui, "词库");
        let n = self.store.n;
        let rewritten = self.store.overlay_len();
        self.row(ui, "词头", |ui| {
            ui.label(self.text(&format!("{n} 条"), fonts::sans(13.0), p.ink_soft));
        });
        self.row(ui, "义项重组", |ui| {
            let t =
                if rewritten > 0 { format!("{rewritten} 条") } else { "未启用".to_string() };
            ui.label(self.text(&t, fonts::sans(13.0), p.ink_soft));
        });
        ui.add_space(30.0);
    }

    /// 捕获新的快捷键组合。至少要一个修饰键，否则会把普通打字全截走。
    fn capture_hotkey(&mut self, ctx: &egui::Context) {
        if !self.capturing_hotkey {
            return;
        }
        let got = ctx.input(|i| {
            let m = i.modifiers;
            let mut mods = 0u32;
            if m.ctrl {
                mods |= crate::tray::MOD_CONTROL;
            }
            if m.alt {
                mods |= crate::tray::MOD_ALT;
            }
            if m.shift {
                mods |= crate::tray::MOD_SHIFT;
            }
            i.events.iter().find_map(|e| match e {
                egui::Event::Key { key, pressed: true, .. } => {
                    vk_of(*key).map(|vk| crate::tray::Hotkey { mods, vk })
                }
                _ => None,
            })
        });
        let Some(hk) = got else { return };
        if !hk.is_valid() {
            self.note = "快捷键至少要带一个 Ctrl / Alt / Shift".into();
            return;
        }
        self.cfg.hotkey = hk;
        self.cfg_dirty = true;
        self.capturing_hotkey = false;
        if let Some(t) = self.tray.as_ref() {
            t.set_hotkey(hk);
        }
    }

    /// 设置子窗口。
    ///
    /// 用 `show_viewport_immediate` 而不是 deferred：deferred 的回调要求
    /// `Fn + Send + Sync + 'static`，就得把设置涉及的一堆状态搬进 Arc<Mutex>；
    /// immediate 的回调能直接借 `&mut self`，这里的状态量不值得为它换架构。
    ///
    /// 代价是子窗口由主窗口这一帧驱动 —— 主窗口藏起来时 egui 不出帧，
    /// 子窗口也就不刷新。所以凡是打开设置的地方都先把主窗口叫出来。
    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let p = self.palette;
        let mut close = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("settings"),
            egui::ViewportBuilder::default()
                .with_title("词典 · 设置")
                .with_inner_size(self.cfg.window)
                .with_min_inner_size(WINDOW_MIN)
                .with_decorations(false),
            |ui, class| {
                let embedded = class == egui::ViewportClass::EmbeddedWindow;
                if self.settings_focus_pending && !embedded {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
                    self.settings_focus_pending = false;
                }
                let whole = ui.max_rect();
                ui.painter().rect_filled(whole, 0.0, p.ground);
                // 内嵌模式（自截图）下子视口画在主窗口里，再画一条标题栏会串味
                let full = if embedded {
                    whole
                } else {
                    let t = Rect::from_min_max(
                        whole.min,
                        Pos2::new(whole.max.x, whole.min.y + TITLE_H),
                    );
                    self.title_bar(ui, t, false);
                    Rect::from_min_max(Pos2::new(whole.min.x, t.max.y), whole.max)
                };
                // 右边多留一点给滚动条，免得分隔线顶到它下面
                let inner = Rect::from_min_max(
                    full.min + Vec2::new(28.0, 0.0),
                    full.max - Vec2::new(16.0, 0.0),
                );
                ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("settings-scroll")
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        // 不加这句的话 ScrollArea 会收缩到内容宽度，
                        // 滚动条跟着贴到内容右边 —— 看起来就是「滚动条在页面中间」
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.settings_page(ui));
                });
                let ctx = ui.ctx();
                // 诊断用：这一步要拍设置窗口的话，请求和收取都得在子视口里做
                if let Some(plan) = self.shot.as_mut() {
                    if plan.current().is_some_and(|s| s.settings) && !embedded {
                        let got: Vec<egui::ColorImage> = ctx.input(|i| {
                            i.events
                                .iter()
                                .filter_map(|e| match e {
                                    egui::Event::Screenshot { image, .. } => {
                                        Some((**image).clone())
                                    }
                                    _ => None,
                                })
                                .collect()
                        });
                        for img in got {
                            let name = plan.current().map(|s| s.name).unwrap_or("settings");
                            let path = plan.dir.join(format!("{name}.bmp"));
                            let [w, h] = img.size;
                            match crate::shot::write_bmp(&path, w, h, img.as_raw()) {
                                Ok(()) => println!("截图 {} ({w}x{h})", path.display()),
                                Err(e) => eprintln!("截图写入失败: {e}"),
                            }
                            plan.step += 1;
                            plan.waited = 0;
                            plan.typed_at = 0;
                            plan.requested = false;
                        }
                        if !plan.requested && plan.waited >= 14 {
                            plan.requested = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                                egui::UserData::default(),
                            ));
                        }
                        ctx.request_repaint();
                    }
                }

                if ctx.input(|i| i.viewport().close_requested())
                    || ctx.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    close = true;
                }
            },
        );
        if close {
            self.settings_open = false;
            self.capturing_hotkey = false;
            self.save_settings();
        }
    }

    /// 诊断用：按 `shot::SCRIPT` 把界面推到每个状态，各截一张图，走完就退出。
    fn drive_screenshots(&mut self, ctx: &egui::Context) {
        // 先收本帧送回来的截图
        let shots: Vec<egui::ColorImage> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some((**image).clone()),
                    _ => None,
                })
                .collect()
        });

        let Some(plan) = self.shot.as_mut() else { return };

        for img in shots {
            let name = plan.current().map(|s| s.name).unwrap_or("x");
            let path = plan.dir.join(format!("{name}.bmp"));
            let [w, h] = img.size;
            match shot::write_bmp(&path, w, h, img.as_raw()) {
                Ok(()) => println!("截图 {} ({w}x{h})", path.display()),
                Err(e) => eprintln!("截图写入失败 {}: {e}", path.display()),
            }
            plan.step += 1;
            plan.waited = 0;
            plan.typed_at = 0;
            plan.requested = false;
        }

        let Some(step) = plan.current() else {
            println!("截图脚本走完，退出");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            self.shot = None;
            return;
        };

        // 首帧字体光栅化很重，第一步多等一会；之后每步等够淡入动画
        let typing = step.typed.map(|t| t.chars().count()).unwrap_or(0)
            + step.keys.map(|k| k.len()).unwrap_or(0);
        let need = if plan.step == 0 { 30 } else { 14 + typing as u32 * 2 };
        plan.waited += 1;
        if plan.waited == 2 {
            // 把界面推到这一步要的状态
            let (q, open, bar) = (step.query, step.open_entry, step.bar);
            self.show_bar = bar;
            self.show_scores = bar;
            if let Some(q) = q {
                self.query = q.to_string();
                self.last_query.clear();
                self.refresh();
            }
            if open {
                self.open(0);
            }
            self.settings_open = step.settings;
        } else if plan.waited > 2 && plan.typed_at < typing {
            // 每两帧一个动作：文本走 TextEdit 的输入路径，按键走全局导航路径
            if plan.waited % 2 == 0 {
                let i = plan.typed_at;
                plan.typed_at += 1;
                if let Some(t) = step.typed {
                    if let Some(c) = t.chars().nth(i) {
                        ctx.input_mut(|q| q.events.push(egui::Event::Text(c.to_string())));
                    }
                } else if let Some(k) = step.keys {
                    let key = match k.as_bytes().get(i) {
                        Some(b'd') => Some(egui::Key::ArrowDown),
                        Some(b'u') => Some(egui::Key::ArrowUp),
                        Some(b'e') => Some(egui::Key::Enter),
                        Some(b'x') => Some(egui::Key::Escape),
                        _ => None,
                    };
                    if let Some(key) = key {
                        ctx.input_mut(|q| {
                            q.events.push(egui::Event::Key {
                                key,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: egui::Modifiers::NONE,
                            })
                        });
                    }
                }
            }
        } else if plan.waited >= need
            && !plan.requested
            && (!step.settings || ctx.embed_viewports())
        {
            // settings 那一步要拍的是子窗口，请求得从子窗口那边发出去，
            // 否则拿到的是主窗口的画面
            plan.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint();
    }

    fn note_strip(&mut self, ui: &mut egui::Ui) {
        let p = self.palette;
        let r = ui.max_rect();
        let note = self.note.clone();
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(r.shrink2(Vec2::new(24.0, 4.0)))
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.label(self.text(&note, fonts::sans(12.0), p.muted));
                if ui.small_button("×").clicked() {
                    self.note.clear();
                }
            },
        );
    }
}

impl eframe::App for App {
    /// 窗口藏起来时 egui 不出帧，`ui()` 不会被调用 —— 托盘和热键的事件
    /// 只能在这里处理，否则按了热键也叫不出窗口。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.start_deferred(ctx);
        // 顺序不能反：托盘的「退出」是靠一条 WM_CLOSE 把界面叫醒的，
        // 得先收到 Quit 把 quitting 立起来，pump_close 才知道这次不是
        // 「关窗口 = 收进托盘」。
        self.pump_tray(ctx);
        self.pump_close(ctx);
    }

    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        let c = self.palette.ground;
        [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.painted = true;
        self.frames += 1;
        if self.shot.is_some() {
            self.drive_screenshots(&ctx);
        }

        self.capture_hotkey(&ctx);
        // 捕获快捷键时不要让主窗口的导航键逻辑插手
        if !self.capturing_hotkey {
            self.handle_keys(&ctx);
        }
        self.save_settings();

        let whole = ui.max_rect();

        // 首帧报一次字体探测结果，字体缺失时好排查
        if !self.font_report.is_empty() {
            println!("字体: {}", self.font_report);
            self.font_report.clear();
        }

        self.remember_size(&ctx);
        let title = Rect::from_min_max(whole.min, Pos2::new(whole.max.x, whole.min.y + TITLE_H));
        self.title_bar(ui, title, true);
        let full = Rect::from_min_max(Pos2::new(whole.min.x, title.max.y), whole.max);

        let bar_h = if self.show_bar { BAR_H } else { 0.0 };
        let note_h = if self.note.is_empty() { 0.0 } else { NOTE_H };
        let split = full.max.y - bar_h - note_h;

        let content = Rect::from_min_max(full.min, Pos2::new(full.max.x, split));
        ui.scope_builder(UiBuilder::new().max_rect(content), |ui| self.column(ui));

        if note_h > 0.0 {
            let r = Rect::from_min_max(
                Pos2::new(full.min.x, split),
                Pos2::new(full.max.x, split + note_h),
            );
            ui.scope_builder(UiBuilder::new().max_rect(r), |ui| self.note_strip(ui));
        }
        if bar_h > 0.0 {
            let r = Rect::from_min_max(Pos2::new(full.min.x, full.max.y - bar_h), full.max);
            ui.scope_builder(UiBuilder::new().max_rect(r), |ui| self.bar(ui));
        }

        self.settings_window(&ctx);
        // 放在最后：见 resize_edges 的说明，早了会被内容吃掉
        ui.scope_builder(UiBuilder::new().max_rect(whole), |ui| self.resize_edges(ui));
    }
}

/// egui 的键 → Win32 虚拟键码。只覆盖快捷键用得上的那些。
fn vk_of(k: egui::Key) -> Option<u32> {
    use egui::Key::*;
    Some(match k {
        Space => 0x20,
        A => 0x41,
        B => 0x42,
        C => 0x43,
        D => 0x44,
        E => 0x45,
        F => 0x46,
        G => 0x47,
        H => 0x48,
        I => 0x49,
        J => 0x4A,
        K => 0x4B,
        L => 0x4C,
        M => 0x4D,
        N => 0x4E,
        O => 0x4F,
        P => 0x50,
        Q => 0x51,
        R => 0x52,
        S => 0x53,
        T => 0x54,
        U => 0x55,
        V => 0x56,
        W => 0x57,
        X => 0x58,
        Y => 0x59,
        Z => 0x5A,
        Num0 => 0x30,
        Num1 => 0x31,
        Num2 => 0x32,
        Num3 => 0x33,
        Num4 => 0x34,
        Num5 => 0x35,
        Num6 => 0x36,
        Num7 => 0x37,
        Num8 => 0x38,
        Num9 => 0x39,
        F1 => 0x70,
        F2 => 0x71,
        F3 => 0x72,
        F4 => 0x73,
        F5 => 0x74,
        F6 => 0x75,
        F7 => 0x76,
        F8 => 0x77,
        F9 => 0x78,
        F10 => 0x79,
        F11 => 0x7A,
        F12 => 0x7B,
        _ => return None,
    })
}

/// 标题栏上那四个图标。都是画出来的 —— 字形按钮的大小和基线受字体摆布，
/// 而这里要的是四个视觉重量一致、能对齐到像素的小记号。
#[derive(Clone, Copy, PartialEq)]
enum Glyph {
    /// 钉在最前。一颗图钉：圆头 + 斜杆。
    Pin,
    Min,
    Max {
        restore: bool,
    },
    Close,
}

impl Glyph {
    fn id(self) -> u8 {
        match self {
            Glyph::Pin => 0,
            Glyph::Min => 1,
            Glyph::Max { .. } => 2,
            Glyph::Close => 3,
        }
    }

    fn paint(self, painter: &egui::Painter, c: Pos2, ink: Color32, on: bool) {
        let st = egui::Stroke::new(1.2, ink);
        match self {
            Glyph::Min => {
                painter.hline((c.x - 5.0)..=(c.x + 5.0), c.y, st);
            }
            Glyph::Max { restore } => {
                if restore {
                    // 还原：两个错开的方块，后面那个只露出上和右两条边
                    let back = Rect::from_min_size(c + Vec2::new(-2.0, -5.0), Vec2::splat(7.0));
                    painter.line_segment([back.left_top(), back.right_top()], st);
                    painter.line_segment([back.right_top(), back.right_bottom()], st);
                    let front = Rect::from_min_size(c + Vec2::new(-5.0, -2.0), Vec2::splat(7.0));
                    painter.rect_stroke(front, 0.0, st, egui::StrokeKind::Inside);
                } else {
                    let r = Rect::from_center_size(c, Vec2::splat(9.0));
                    painter.rect_stroke(r, 0.0, st, egui::StrokeKind::Inside);
                }
            }
            Glyph::Close => {
                let d = 4.5;
                painter.line_segment([c + Vec2::new(-d, -d), c + Vec2::new(d, d)], st);
                painter.line_segment([c + Vec2::new(-d, d), c + Vec2::new(d, -d)], st);
            }
            Glyph::Pin => {
                // 一颗图钉：横着的帽子 + 往下的针。
                // 钉住时立起来、帽子填实；没钉住时歪着、帽子只描边 ——
                // 姿势本身就是状态，不必靠颜色深浅去猜（画圆点会被看成放大镜）。
                let ang: f32 = if on { 0.0 } else { -0.72 };
                let (sa, ca) = ang.sin_cos();
                let at = |x: f32, y: f32| c + Vec2::new(x * ca - y * sa, x * sa + y * ca);
                let head = vec![at(-4.6, -4.8), at(4.6, -4.8), at(4.6, -1.9), at(-4.6, -1.9)];
                let fill = if on { ink } else { Color32::TRANSPARENT };
                painter.add(egui::Shape::convex_polygon(head, fill, st));
                painter.line_segment([at(0.0, -1.9), at(0.0, 5.4)], st);
            }
        }
    }
}

fn params_eq(a: &Params, b: &Params) -> bool {
    a.exact_bonus == b.exact_bonus
        && a.lambda == b.lambda
        && a.secondary == b.secondary
        && a.max_secondary == b.max_secondary
        && a.limit == b.limit
}

#[cfg(test)]
mod tests {
    use super::{decide_close, CloseAct};

    /// 「退出」发的 Close 不能被「关窗口 = 收进托盘」拦下来。
    #[test]
    fn quitting_is_not_swallowed_by_the_close_to_tray_rule() {
        assert_eq!(decide_close(true, true, true), CloseAct::Exit, "菜单点了退出");
        assert_eq!(decide_close(false, true, true), CloseAct::ToTray, "点的是关闭按钮");
    }

    /// 没有托盘可收、或者用户关掉了这个行为时，关窗口就是退出。
    #[test]
    fn without_a_tray_to_hide_into_closing_really_closes() {
        assert_eq!(decide_close(false, true, false), CloseAct::Exit, "托盘起不来");
        assert_eq!(decide_close(false, false, true), CloseAct::Exit, "用户关掉了收进托盘");
    }
}
