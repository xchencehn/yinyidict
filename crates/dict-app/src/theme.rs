//! 配色。
//!
//! 全局只有声调这一处用色，其余是墨色和灰。颜色的有无本身就是类型信号 ——
//! 中文词条有彩色注音，英文词条没有，不需要额外加标签。

use egui::{Color32, Context, Visuals};

fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub ground: Color32,
    pub ink: Color32,
    pub ink_soft: Color32,
    pub muted: Color32,
    pub faint: Color32,
    /// 比 `faint` 还淡一档，只给输入框的占位文字用 —— 它必须一眼能和
    /// 真正输进去的字区分开，不然会以为词已经在框里了。
    pub ghost: Color32,
    pub rule: Color32,
    pub hit: Color32,
    /// 声调 0..4：轻声灰 / 深玫瑰 / 琥珀 / 苔绿 / 靛蓝
    pub tone: [Color32; 5],
}

impl Palette {
    pub fn light() -> Self {
        Palette {
            ground: rgb(0xFBFAF8),
            ink: rgb(0x1A1A19),
            ink_soft: rgb(0x4A4844),
            muted: rgb(0x8A8781),
            faint: rgb(0xB5B2AB),
            ghost: rgb(0xD6D3CC),
            rule: rgb(0xE4E1DB),
            hit: rgb(0xEFEDE7),
            tone: [rgb(0x9A9793), rgb(0xB03A48), rgb(0xB4712A), rgb(0x4B7A3F), rgb(0x3A5F9E)],
        }
    }

    pub fn dark() -> Self {
        Palette {
            ground: rgb(0x16161A),
            ink: rgb(0xEDEBE7),
            ink_soft: rgb(0xC4C1BA),
            muted: rgb(0x87847E),
            faint: rgb(0x5C5A56),
            ghost: rgb(0x3D3B38),
            rule: rgb(0x2B2B31),
            hit: rgb(0x232329),
            tone: [rgb(0x8A8782), rgb(0xE0757F), rgb(0xDCA35B), rgb(0x84B673), rgb(0x7CA0DC)],
        }
    }

    pub fn of(dark: bool) -> Self {
        if dark {
            Self::dark()
        } else {
            Self::light()
        }
    }

    /// 声调色。索引越界（不该发生）时退回轻声灰。
    pub fn tone_of(&self, t: u8) -> Color32 {
        *self.tone.get(t as usize).unwrap_or(&self.tone[0])
    }
}

/// 无卡片、无边框、无阴影 —— 把 egui 默认的那套装饰全部拆掉。
pub fn apply(ctx: &Context, p: &Palette, dark: bool) {
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = p.ground;
    v.window_fill = p.ground;
    v.extreme_bg_color = p.ground;
    v.faint_bg_color = p.hit;
    v.override_text_color = Some(p.ink);
    v.window_stroke = egui::Stroke::NONE;
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;
    v.window_corner_radius = egui::CornerRadius::ZERO;

    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = egui::CornerRadius::same(3);
        w.expansion = 0.0;
    }
    // 无卡片、无边框：只有「非交互」这一档去掉描边。
    // 交互控件的描边要留着 —— 复选框的方框和滑块的轨道都是用它画的，
    // 一起抹掉的话底部调参条就只剩一堆浮着的数字。
    v.widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, p.rule);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, p.muted);
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0, p.ink_soft);
    v.widgets.open.bg_stroke = egui::Stroke::new(1.0, p.rule);
    v.widgets.noninteractive.bg_fill = p.ground;
    v.widgets.noninteractive.weak_bg_fill = p.ground;
    // bg_fill 画的是滑块轨道和复选框底；设成背景色就等于把它们抹掉了。
    // 按钮底走的是 weak_bg_fill，所以这两个要分开给。
    v.widgets.inactive.bg_fill = p.hit;
    v.widgets.inactive.weak_bg_fill = p.ground;
    v.widgets.hovered.bg_fill = p.hit;
    v.widgets.hovered.weak_bg_fill = p.hit;
    v.widgets.active.bg_fill = p.hit;
    v.widgets.active.weak_bg_fill = p.hit;
    v.selection.bg_fill = p.hit;
    v.selection.stroke = egui::Stroke::new(1.0, p.ink);

    ctx.set_visuals(v);

    // 明暗两套 style 一起改，切配色时不用重来一遍
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(6.0, 3.0);
        style.spacing.slider_width = 70.0;
        style.interaction.selectable_labels = false;
    });
}
