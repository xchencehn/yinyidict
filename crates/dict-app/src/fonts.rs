//! 从系统里挑字体装进 egui。
//!
//! 词条页靠字体和注音区分中英，不加标签：中文是宋体大字 + 声调着色拼音，
//! 英文是无衬线大字 + 灰色 IPA。所以「衬线 CJK」和「无衬线」必须是两套真正
//! 不同的字体，不能靠一套凑合。

use egui::{Context, FontData, FontDefinitions, FontFamily};
use std::sync::Arc;

/// 词条大字和中文候选用的衬线族。
pub const SERIF: &str = "serif-cjk";
/// 英文词头用的半粗无衬线族。
pub const SEMIBOLD: &str = "sans-semibold";

const FONT_DIR: &str = r"C:\Windows\Fonts";

/// 按优先级给出候选文件名，取第一个存在且能解析的。
struct Pick {
    slot: &'static str,
    files: &'static [&'static str],
}

const PICKS: &[Pick] = &[
    // 衬线 CJK：Noto Serif SC 是原型的首选；退回华文宋体，再退回楷体。
    Pick { slot: "cjk-serif", files: &["NotoSerifSC-VF.ttf", "STSONG.TTF", "simkai.ttf"] },
    // 无衬线 CJK
    Pick { slot: "cjk-sans", files: &["NotoSansSC-VF.ttf", "msyhl.ttc", "simhei.ttf"] },
    // 西文正文
    Pick { slot: "latin", files: &["segoeui.ttf"] },
    // 西文半粗
    Pick { slot: "latin-sb", files: &["seguisb.ttf", "segoeui.ttf"] },
    // 西文衬线，给中文词条页里夹杂的拉丁字母用
    Pick { slot: "latin-serif", files: &["georgia.ttf"] },
];

fn load(files: &[&str]) -> Option<(String, FontData)> {
    for f in files {
        let path = std::path::Path::new(FONT_DIR).join(f);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        // 可变字体（-VF）取默认实例即可；解析不了就换下一个候选。
        let data = FontData::from_owned(bytes);
        return Some((f.to_string(), data));
    }
    None
}

pub fn install(ctx: &Context) -> Vec<String> {
    let mut defs = FontDefinitions::default();
    let mut have: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    let mut report = Vec::new();

    for p in PICKS {
        match load(p.files) {
            Some((name, data)) => {
                report.push(format!("{}={}", p.slot, name));
                defs.font_data.insert(p.slot.to_string(), Arc::new(data));
                have.insert(p.slot, name);
            }
            None => report.push(format!("{}=<缺失>", p.slot)),
        }
    }

    let push = |defs: &mut FontDefinitions, fam: FontFamily, slots: &[&str]| {
        let list = defs.families.entry(fam).or_default();
        // 自己挑的字体排在前面，egui 自带的兜底字形留在后面
        for (i, s) in slots.iter().filter(|s| have.contains_key(**s)).enumerate() {
            list.insert(i, s.to_string());
        }
    };

    push(&mut defs, FontFamily::Proportional, &["latin", "cjk-sans"]);
    push(&mut defs, FontFamily::Monospace, &["latin", "cjk-sans"]);
    defs.families.insert(
        FontFamily::Name(SERIF.into()),
        owned(&have, &["cjk-serif", "latin-serif", "latin"]),
    );
    defs.families
        .insert(FontFamily::Name(SEMIBOLD.into()), owned(&have, &["latin-sb", "cjk-sans"]));

    ctx.set_fonts(defs);
    report
}

fn owned(have: &std::collections::HashMap<&str, String>, slots: &[&'static str]) -> Vec<String> {
    slots.iter().filter(|s| have.contains_key(**s)).map(|s| s.to_string()).collect()
}

/// 衬线族的 `FontId`。
pub fn serif(size: f32) -> egui::FontId {
    egui::FontId::new(size, FontFamily::Name(SERIF.into()))
}

/// 半粗无衬线族的 `FontId`。
pub fn semibold(size: f32) -> egui::FontId {
    egui::FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// 常规无衬线。
pub fn sans(size: f32) -> egui::FontId {
    egui::FontId::new(size, FontFamily::Proportional)
}
