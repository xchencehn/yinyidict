//! CC-CEDICT 的数字调拼音 → 带调拼音 / 无调拼音 / 首字母。
//!
//! 注音一律取自 CC-CEDICT 的**词级**标注，不做字级映射再拼合 ——
//! 「银行 yin2 hang2」这类多音字只有词级标注才是对的。

/// 单个音节按声调加变音符号后的形式。索引 [元音][声调-1]。
const MARKS: [(char, [char; 4]); 6] = [
    ('a', ['\u{101}', '\u{e1}', '\u{1ce}', '\u{e0}']),
    ('o', ['\u{14d}', '\u{f3}', '\u{1d2}', '\u{f2}']),
    ('e', ['\u{113}', '\u{e9}', '\u{11b}', '\u{e8}']),
    ('i', ['\u{12b}', '\u{ed}', '\u{1d0}', '\u{ec}']),
    ('u', ['\u{16b}', '\u{fa}', '\u{1d4}', '\u{f9}']),
    ('\u{fc}', ['\u{1d6}', '\u{1d8}', '\u{1da}', '\u{1dc}']),
];

fn mark(c: char, tone: u8) -> char {
    if !(1..=4).contains(&tone) {
        return c;
    }
    MARKS.iter().find(|(base, _)| *base == c).map(|(_, m)| m[(tone - 1) as usize]).unwrap_or(c)
}

/// 一个已拆分的音节。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Syllable {
    /// 带调形式，如 `zhōng`。非拼音记号（如 `A`、`·`）原样保留。
    pub toned: String,
    /// 无调小写形式，如 `zhong`。用于 `py` 通道。
    pub plain: String,
    /// `plain` 中 ü 写成 `v` 的变体；没有 ü 时与 `plain` 相同。用于兼容 `nv`/`lv` 输入法习惯。
    pub plain_v: String,
    pub tone: u8,
}

/// 拆一个 CC-CEDICT 音节记号，如 `zhong1` / `nu:3` / `r5` / `A`。
pub fn parse_syllable(tok: &str) -> Syllable {
    let mut body = tok;
    let mut tone = 0u8;
    if let Some(last) = tok.chars().last() {
        if let Some(d) = last.to_digit(10) {
            if (1..=5).contains(&d) {
                tone = if d == 5 { 0 } else { d as u8 };
                body = &tok[..tok.len() - last.len_utf8()];
            }
        }
    }
    // CC-CEDICT 用 `u:` 表示 ü
    let body = body.replace("u:", "\u{fc}").replace("U:", "\u{dc}");

    let lower = body.to_lowercase();
    let plain = lower.replace('\u{fc}', "u");
    let plain_v = lower.replace('\u{fc}', "v");

    // 声调符号落点：有 a 标 a；否则有 o 或 e 标它；否则标最后一个元音。
    let toned = if tone == 0 || !lower.chars().any(|c| MARKS.iter().any(|(b, _)| *b == c)) {
        body.clone()
    } else {
        let chars: Vec<char> = lower.chars().collect();
        let pos = chars
            .iter()
            .position(|&c| c == 'a')
            .or_else(|| chars.iter().position(|&c| c == 'o'))
            .or_else(|| chars.iter().position(|&c| c == 'e'))
            .or_else(|| chars.iter().rposition(|&c| MARKS.iter().any(|(b, _)| *b == c)));
        match pos {
            Some(i) => {
                let mut out: Vec<char> = chars.clone();
                out[i] = mark(out[i], tone);
                let s: String = out.into_iter().collect();
                // 还原原始大小写（专有名词首字母大写）
                if body.chars().next().is_some_and(|c| c.is_uppercase()) {
                    let mut it = s.chars();
                    match it.next() {
                        Some(f) => f.to_uppercase().collect::<String>() + it.as_str(),
                        None => s,
                    }
                } else {
                    s
                }
            }
            None => body.clone(),
        }
    };

    Syllable { toned, plain, plain_v, tone }
}

/// 整串读音，如 `"Zhong1 guo2"`。
#[derive(Clone, Debug, Default)]
pub struct Reading {
    pub syllables: Vec<Syllable>,
}

impl Reading {
    pub fn parse(s: &str) -> Self {
        Reading { syllables: s.split_whitespace().map(parse_syllable).collect() }
    }
    /// 空格分隔的带调拼音，用于展示。
    pub fn toned(&self) -> String {
        self.syllables.iter().map(|s| s.toned.as_str()).collect::<Vec<_>>().join(" ")
    }
    pub fn tones(&self) -> Vec<u8> {
        self.syllables.iter().map(|s| s.tone).collect()
    }
    /// 连写的无调拼音，用于 `py` 通道：`zhongguo`。
    pub fn plain(&self) -> String {
        self.syllables.concat_plain(false)
    }
    /// ü→v 变体。与 `plain()` 相同时调用方应跳过重复插入。
    pub fn plain_v(&self) -> String {
        self.syllables.concat_plain(true)
    }
    /// 首字母串，用于 `ini` 通道：`zg`。
    pub fn initials(&self) -> String {
        self.syllables.iter().filter_map(|s| s.plain.chars().next()).collect()
    }
}

trait ConcatPlain {
    fn concat_plain(&self, v: bool) -> String;
}
impl ConcatPlain for Vec<Syllable> {
    fn concat_plain(&self, v: bool) -> String {
        let mut out = String::new();
        for s in self {
            out.push_str(if v { &s.plain_v } else { &s.plain });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_placement() {
        assert_eq!(parse_syllable("zhong1").toned, "zh\u{14d}ng");
        assert_eq!(parse_syllable("guo2").toned, "gu\u{f3}");
        assert_eq!(parse_syllable("hao3").toned, "h\u{1ce}o");
        assert_eq!(parse_syllable("mai4").toned, "m\u{e0}i");
        // iu → 标在 u，ui → 标在 i
        assert_eq!(parse_syllable("liu2").toned, "li\u{fa}");
        assert_eq!(parse_syllable("gui4").toned, "gu\u{ec}");
        // 轻声不加符号
        assert_eq!(parse_syllable("jiu5").toned, "jiu");
        assert_eq!(parse_syllable("jiu5").tone, 0);
    }

    #[test]
    fn umlaut_and_case() {
        let s = parse_syllable("nu:3");
        assert_eq!(s.toned, "n\u{1da}");
        assert_eq!(s.plain, "nu");
        assert_eq!(s.plain_v, "nv");
        assert_eq!(parse_syllable("Zhong1").toned, "Zh\u{14d}ng");
    }

    #[test]
    fn non_pinyin_tokens_pass_through() {
        assert_eq!(parse_syllable("A").toned, "A");
        assert_eq!(parse_syllable("\u{b7}").toned, "\u{b7}");
    }

    #[test]
    fn whole_reading() {
        let r = Reading::parse("Zhong1 guo2");
        assert_eq!(r.toned(), "Zh\u{14d}ng gu\u{f3}");
        assert_eq!(r.plain(), "zhongguo");
        assert_eq!(r.initials(), "zg");
        assert_eq!(r.tones(), vec![1, 2]);
        // 词级消歧：银行 = yin2 hang2，不是 yin2 xing2
        let r = Reading::parse("yin2 hang2");
        assert_eq!(r.toned(), "y\u{ed}n h\u{e1}ng");
    }
}
