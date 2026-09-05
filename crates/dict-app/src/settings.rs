//! 设置的持久化。
//!
//! 存成工程根下的 `settings.json`，跟 `data/` 放在一起 —— 这是个便携程序，
//! 整个目录拷走就能用，不往注册表或 AppData 里塞东西。
//!
//! 任何一个字段读坏了都退回默认值而不是整份放弃：设置文件损坏不该让人打不开词典。

use crate::tray::Hotkey;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Auto,
    Light,
    Dark,
}

impl Theme {
    pub fn label(self) -> &'static str {
        match self {
            Theme::Auto => "跟随系统",
            Theme::Light => "浅色",
            Theme::Dark => "深色",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub hotkey: Hotkey,
    /// 窗口大小（逻辑点，不含标题栏之外的边框）。**每次改动都会记下来** ——
    /// 不然每次启动都得重新拖一遍。最大化时不记，否则还原不回去。
    pub window: [f32; 2],
    /// 钉在最前。
    pub pinned: bool,
    /// 关窗口时收进托盘而不是退出。
    pub close_to_tray: bool,
    /// 启动时不显示窗口，只驻留托盘 —— 配合开机自启用。
    pub start_hidden: bool,
    pub theme: Theme,

    pub speed: f32,
    /// 载体句合成再裁剪。关掉就是直接喂单词，留着做 A/B。
    pub carrier: bool,

    pub exact_bonus: f32,
    pub lambda: f32,
    pub secondary: f32,
    pub max_secondary: usize,
    pub stable_order: bool,
    pub show_scores: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let p = dict_core::Params::default();
        Settings {
            hotkey: Hotkey::default(),
            window: crate::app::WINDOW_DEFAULT,
            pinned: false,
            close_to_tray: true,
            start_hidden: false,
            theme: Theme::Auto,
            speed: 1.0,
            carrier: true,
            exact_bonus: p.exact_bonus,
            lambda: p.lambda,
            secondary: p.secondary,
            max_secondary: p.max_secondary,
            stable_order: true,
            show_scores: false,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Settings::default();
        };
        match serde_json::from_str::<Settings>(&text) {
            Ok(s) => s.sanitized(),
            Err(e) => {
                eprintln!("设置文件读不出来（{e}），用默认值：{}", path.display());
                Settings::default()
            }
        }
    }

    pub fn save(&self, path: &Path) {
        match serde_json::to_string_pretty(self) {
            Ok(t) => {
                if let Err(e) = std::fs::write(path, t) {
                    eprintln!("设置写入失败 {}: {e}", path.display());
                }
            }
            Err(e) => eprintln!("设置序列化失败: {e}"),
        }
    }

    /// 手改过文件的话值可能离谱，夹回可用范围。
    fn sanitized(mut self) -> Settings {
        let d = Settings::default();
        if !self.hotkey.is_valid() {
            self.hotkey = d.hotkey;
        }
        // 手改文件写了个离谱的尺寸，或者上次退出时窗口正好被拖到极小，
        // 都不该让下次启动开出一个点不开的窗口
        let d = crate::app::WINDOW_DEFAULT;
        let min = crate::app::WINDOW_MIN;
        for ((cur, def), lo) in self.window.iter_mut().zip(d).zip(min) {
            if !cur.is_finite() || *cur < lo {
                *cur = def;
            }
            *cur = cur.min(4000.0);
        }
        self.speed = self.speed.clamp(0.5, 2.0);
        self.exact_bonus = self.exact_bonus.clamp(0.0, 24.0);
        self.lambda = self.lambda.clamp(0.0, 4.0);
        self.secondary = self.secondary.clamp(0.0, 1.0);
        self.max_secondary = self.max_secondary.min(8);
        self
    }

    pub fn params(&self) -> dict_core::Params {
        dict_core::Params {
            exact_bonus: self.exact_bonus,
            lambda: self.lambda,
            secondary: self.secondary,
            max_secondary: self.max_secondary,
            ..dict_core::Params::default()
        }
    }

    pub fn apply_params(&mut self, p: &dict_core::Params) {
        self.exact_bonus = p.exact_bonus;
        self.lambda = p.lambda;
        self.secondary = p.secondary;
        self.max_secondary = p.max_secondary;
    }
}

/// 设置文件的位置：优先工程根，其次可执行文件旁边。
pub fn path(roots: &[PathBuf]) -> PathBuf {
    roots
        .iter()
        .map(|r| r.join("settings.json"))
        .find(|p| p.exists())
        .or_else(|| roots.first().map(|r| r.join("settings.json")))
        .unwrap_or_else(|| PathBuf::from("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let s = Settings { speed: 1.25, theme: Theme::Dark, ..Default::default() };
        let text = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.theme, Theme::Dark);
        assert!((back.speed - 1.25).abs() < 1e-6);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // 老版本写的文件、或者手改漏了字段，不该让整份设置作废。
        // 这里那两个字段是上一版才有的（音色），现在多出来也不能让文件作废
        let s: Settings = serde_json::from_str(r#"{"speed": 1.5, "speaker_zh": 7}"#).unwrap();
        assert!((s.speed - 1.5).abs() < 1e-6);
        assert_eq!(s.theme, Theme::Auto);
        assert!(s.close_to_tray);
        assert_eq!(s.hotkey, Hotkey::default());
    }

    #[test]
    fn an_unusable_window_size_falls_back_to_the_default() {
        let tiny: Settings = serde_json::from_str(r#"{"window": [10.0, 8.0]}"#).unwrap();
        assert_eq!(
            tiny.sanitized().window,
            crate::app::WINDOW_DEFAULT,
            "小到点不开的窗口要退回默认"
        );

        let ok: Settings = serde_json::from_str(r#"{"window": [900.0, 700.0]}"#).unwrap();
        assert_eq!(ok.sanitized().window, [900.0, 700.0], "正常尺寸要原样留着");
    }

    #[test]
    fn the_window_size_survives_a_round_trip() {
        let s = Settings { window: [458.0, 632.0], pinned: true, ..Default::default() };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.window, [458.0, 632.0]);
        assert!(back.pinned);
    }

    #[test]
    fn absurd_values_get_clamped() {
        let s: Settings =
            serde_json::from_str(r#"{"speed": 99.0, "lambda": -5.0, "max_secondary": 500}"#)
                .unwrap();
        let s = s.sanitized();
        assert_eq!(s.speed, 2.0);
        assert_eq!(s.lambda, 0.0);
        assert_eq!(s.max_secondary, 8);
    }

    #[test]
    fn an_invalid_hotkey_reverts_to_the_default() {
        let s: Settings = serde_json::from_str(r#"{"hotkey":{"mods":0,"vk":68}}"#).unwrap();
        assert_eq!(s.sanitized().hotkey, Hotkey::default());
    }

    #[test]
    fn a_corrupt_file_does_not_take_the_dictionary_down_with_it() {
        let p = std::env::temp_dir().join(format!("dict-bad-{}.json", std::process::id()));
        std::fs::write(&p, "{ this is not json").unwrap();
        let s = Settings::load(&p);
        assert_eq!(s.hotkey, Hotkey::default());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn params_survive_a_round_trip_through_settings() {
        let mut s = Settings::default();
        let p = dict_core::Params { exact_bonus: 9.5, lambda: 0.8, ..Default::default() };
        s.apply_params(&p);
        let back = s.params();
        assert_eq!(back.exact_bonus, 9.5);
        assert_eq!(back.lambda, 0.8);
    }
}
