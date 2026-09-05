//! 自截图：让程序把自己渲染出的帧缓冲写成 BMP。
//!
//! 为什么需要：这台机器上外部截图抓不到本窗口 —— `CopyFromScreen` 只拿到壁纸，
//! `PrintWindow` 能拿到边框但拿不到 OpenGL 客户区。要确认界面真的画对了，
//! 只能从 egui 自己的帧缓冲取。调 UI 时这条路一直有用。
//!
//! 用法：设环境变量 `DICT_SHOT=<输出目录>` 再启动，程序会按脚本走一遍
//! 「空 → 候选 → 词条」三态，每步存一张图，然后退出。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 脚本里的一步。
pub struct Step {
    /// 这一步要把输入框设成什么；`None` 表示不改。
    pub query: Option<&'static str>,
    /// 是否把当前选中的候选打开成词条页。
    pub open_entry: bool,
    /// 是否展开底部调参条（平时按 F1 唤出）。
    pub bar: bool,
    /// 逐字符喂进 egui 事件队列，走真正的 TextEdit 输入路径。
    /// 直接改 `query` 字段验证不到 `resp.changed()` 和逐击键的稳定排序。
    pub typed: Option<&'static str>,
    /// 按键脚本：`d`=↓ `u`=↑ `e`=回车 `x`=Esc。每两帧按一个。
    pub keys: Option<&'static str>,
    /// 把面板切到设置页。
    pub settings: bool,
    pub name: &'static str,
}

/// 走一遍三态，覆盖中文词头、英文词头、词条页。
pub const SCRIPT: &[Step] = &[
    Step {
        query: None,
        open_entry: false,
        bar: false,
        typed: None,
        keys: None,
        settings: false,
        name: "1-空",
    },
    Step {
        query: Some("中国"),
        open_entry: false,
        bar: false,
        typed: None,
        keys: None,
        settings: false,
        name: "2-中文候选",
    },
    Step {
        query: Some("中国"),
        open_entry: true,
        bar: false,
        typed: None,
        keys: None,
        settings: false,
        name: "3-中文词条",
    },
    Step {
        query: Some("zhonggu"),
        open_entry: false,
        bar: false,
        typed: None,
        keys: None,
        settings: false,
        name: "4-拼音候选",
    },
    Step {
        query: Some("compile"),
        open_entry: false,
        bar: false,
        typed: None,
        keys: None,
        settings: false,
        name: "5-英文候选",
    },
    Step {
        query: Some("compile"),
        open_entry: true,
        bar: false,
        typed: None,
        keys: None,
        settings: false,
        name: "6-英文词条",
    },
    Step {
        query: Some("意思"),
        open_entry: true,
        bar: false,
        typed: None,
        keys: None,
        settings: false,
        name: "7-已重组词条",
    },
    Step {
        query: Some("man"),
        open_entry: false,
        bar: true,
        typed: None,
        keys: None,
        settings: false,
        name: "8-调参条与打分",
    },
    // 走真实输入路径：逐字符敲 zhongwen，验证 resp.changed() 和逐击键的稳定排序
    Step {
        query: Some(""),
        open_entry: false,
        bar: false,
        typed: Some("zhongwen"),
        keys: None,
        settings: false,
        name: "9-逐字符输入",
    },
    // 返回路径：↓↓ 选到第 3 项
    Step {
        query: Some("中国"),
        open_entry: false,
        bar: false,
        typed: None,
        keys: Some("dd"),
        settings: false,
        name: "10-方向键选中第3项",
    },
    // 回车打开它
    Step {
        query: None,
        open_entry: false,
        bar: false,
        typed: None,
        keys: Some("e"),
        settings: false,
        name: "11-回车进词条",
    },
    // 再按 ↓ 退回候选，且**保留原选中项**（应仍高亮第 3 项）
    Step {
        query: None,
        open_entry: false,
        bar: false,
        typed: None,
        keys: Some("d"),
        settings: false,
        name: "12-方向键退回并保留选中",
    },
    Step {
        query: Some(""),
        open_entry: false,
        bar: false,
        typed: None,
        keys: None,
        settings: true,
        name: "13-设置页",
    },
];

pub struct Plan {
    pub dir: PathBuf,
    pub step: usize,
    /// `typed` 已经喂进去几个字符。
    pub typed_at: usize,
    /// 当前步已经等了几帧。切换内容后要留几帧给字体光栅化和淡入动画。
    pub waited: u32,
    pub requested: bool,
}

impl Plan {
    /// 只有设了 `DICT_SHOT` 才启用。
    pub fn from_env() -> Option<Plan> {
        let dir = PathBuf::from(std::env::var("DICT_SHOT").ok()?);
        std::fs::create_dir_all(&dir).ok()?;
        Some(Plan { dir, step: 0, typed_at: 0, waited: 0, requested: false })
    }

    pub fn current(&self) -> Option<&'static Step> {
        SCRIPT.get(self.step)
    }
}

/// 32 位 BGRA、自上而下的 BMP。选 BMP 是因为不需要压缩和 CRC，
/// 几十行就能写完，不必为诊断代码引入图像库。
pub fn write_bmp(path: &Path, w: usize, h: usize, rgba: &[u8]) -> std::io::Result<()> {
    let stride = w * 4;
    let pixels = stride * h;
    let file_size = 14 + 40 + pixels;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);

    f.write_all(b"BM")?;
    f.write_all(&(file_size as u32).to_le_bytes())?;
    f.write_all(&0u16.to_le_bytes())?;
    f.write_all(&0u16.to_le_bytes())?;
    f.write_all(&54u32.to_le_bytes())?; // 像素数据偏移

    f.write_all(&40u32.to_le_bytes())?; // BITMAPINFOHEADER 长度
    f.write_all(&(w as i32).to_le_bytes())?;
    f.write_all(&(-(h as i32)).to_le_bytes())?; // 负高度 = 自上而下
    f.write_all(&1u16.to_le_bytes())?; // 平面数
    f.write_all(&32u16.to_le_bytes())?; // 位深
    f.write_all(&0u32.to_le_bytes())?; // BI_RGB，不压缩
    f.write_all(&(pixels as u32).to_le_bytes())?;
    for _ in 0..4 {
        f.write_all(&0u32.to_le_bytes())?;
    }

    // egui 给的是 RGBA，BMP 要 BGRA
    let mut row = vec![0u8; stride];
    for y in 0..h {
        let src = &rgba[y * stride..y * stride + stride];
        for (d, s) in row.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
        f.write_all(&row)?;
    }
    f.flush()
}
