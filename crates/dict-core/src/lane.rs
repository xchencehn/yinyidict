//! 检索通道。每次击键所有通道全跑再归并 —— 不做通道检测分支。
//!
//! ASCII 输入天然歧义（`man` 既是英文词也是 màn/mǎn 的拼音），
//! 分支一定会漏。只有输入含 CJK 字符时才能确定地关掉英文侧通道。

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Lane {
    /// 中文词头前缀
    Zh,
    /// 中文词头内部子串（词头的真后缀索引）
    ZhSub,
    /// 英文词头前缀
    Enw,
    /// 无调拼音串 `zhonggu`
    Py,
    /// 拼音首字母串 `zg`
    Ini,
    /// 中文词的英文释义（二等）
    Gloss,
    /// 英文词的中文释义（二等）
    Trans,
}

pub const LANES: [Lane; 7] =
    [Lane::Zh, Lane::ZhSub, Lane::Enw, Lane::Py, Lane::Ini, Lane::Gloss, Lane::Trans];

impl Lane {
    pub fn idx(self) -> usize {
        match self {
            Lane::Zh => 0,
            Lane::ZhSub => 1,
            Lane::Enw => 2,
            Lane::Py => 3,
            Lane::Ini => 4,
            Lane::Gloss => 5,
            Lane::Trans => 6,
        }
    }

    pub fn file_stem(self) -> &'static str {
        match self {
            Lane::Zh => "zh",
            Lane::ZhSub => "zhsub",
            Lane::Enw => "enw",
            Lane::Py => "py",
            Lane::Ini => "ini",
            Lane::Gloss => "gloss",
            Lane::Trans => "trans",
        }
    }

    /// 词头命中给满额，释义命中给半额 —— 这是「英文输入优先给英文词」的落点。
    /// 否则打 `china` 会因为「中国」词频高而永远拿不到英文的 china（瓷器）。
    pub fn weight(self) -> f32 {
        match self {
            Lane::Zh => 1.00,
            Lane::Enw => 1.00,
            Lane::Py => 0.95,
            Lane::ZhSub => 0.85,
            Lane::Ini => 0.70,
            Lane::Gloss => 0.50,
            Lane::Trans => 0.50,
        }
    }

    /// 二等通道（释义路）。exact_bonus 要按「释义路权重」折算。
    pub fn is_secondary(self) -> bool {
        matches!(self, Lane::Gloss | Lane::Trans)
    }

    /// 子串命中不该拿完整前缀命中的奖励。
    pub fn allows_exact_bonus(self) -> bool {
        !matches!(self, Lane::ZhSub)
    }

    pub fn label(self) -> &'static str {
        match self {
            Lane::Zh => "汉字",
            Lane::ZhSub => "汉字中",
            Lane::Enw => "英文",
            Lane::Py => "全拼",
            Lane::Ini => "简拼",
            Lane::Gloss => "释义→中",
            Lane::Trans => "释义→英",
        }
    }
}
