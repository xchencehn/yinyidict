//! 词频、异体字、例句 —— 三个给词条「补肉」的辅料源。

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

fn lines_of(path: &Path) -> Result<impl Iterator<Item = String>> {
    let f = File::open(path).with_context(|| format!("打不开 {}", path.display()))?;
    Ok(BufReader::with_capacity(1 << 20, f).lines().map_while(Result::ok))
}

// ─────────────────────────── 中文词频 + 词性 ───────────────────────────

/// jieba 词典：`词 语料计数 词性`。
///
/// 原设计里中文侧词频用 SUBTLEX-CH，但它的官方下载已失效（返回 HTML）。
/// jieba 的计数同样是大规模语料统计量，覆盖 34.9 万词，量纲一致 —— 反正
/// 运行期只比百分位，用哪份语料不影响打分正确性。
pub struct ZhFreq {
    pub count: HashMap<String, f64>,
    pub pos: HashMap<String, String>,
}

/// jieba 的词性代号 → 词条页上显示的中文词性。
fn pos_label(tag: &str) -> &'static str {
    match tag {
        "n" | "nr" | "ns" | "nt" | "nz" | "nrt" | "nrfg" | "ng" => "名",
        "v" | "vd" | "vn" | "vf" | "vx" | "vi" | "vl" | "vg" => "动",
        "a" | "ad" | "an" | "ag" | "al" => "形",
        "d" | "df" | "dg" => "副",
        "m" | "mq" => "数",
        "q" => "量",
        "r" | "rr" | "rz" | "rt" | "ry" | "rg" => "代",
        "p" | "pba" | "pbei" => "介",
        "c" | "cc" => "连",
        "u" | "uj" | "ud" | "ug" | "ul" | "uv" | "uz" => "助",
        "i" => "成语",
        "l" => "习语",
        "j" => "简称",
        "t" | "tg" => "时间",
        "s" => "处所",
        "f" => "方位",
        "b" => "区别",
        "z" => "状态",
        "y" => "语气",
        "e" => "叹",
        "o" => "拟声",
        "h" => "前缀",
        "k" => "后缀",
        "x" | "eng" => "",
        _ => "",
    }
}

impl ZhFreq {
    pub fn load(path: &Path) -> Result<Self> {
        let mut count = HashMap::with_capacity(360_000);
        let mut pos = HashMap::with_capacity(360_000);
        for line in lines_of(path)? {
            let mut it = line.split_whitespace();
            let (Some(w), Some(c)) = (it.next(), it.next()) else { continue };
            let Ok(c) = c.parse::<f64>() else { continue };
            count.insert(w.to_string(), c);
            if let Some(tag) = it.next() {
                let label = pos_label(tag);
                if !label.is_empty() {
                    pos.insert(w.to_string(), label.to_string());
                }
            }
        }
        Ok(ZhFreq { count, pos })
    }
}

// ─────────────────────────── 异体字归一 ───────────────────────────

/// Unihan 异体字表：字 → 异体字。
///
/// 不做这一步查「著/着」「裡/里」会漏 —— 用户按其中一种写法输入，
/// 词头存的是另一种。
///
/// 注意字段的选择：`kZVariant` 在现行 Unihan 里只剩 149 条，已经是遗留字段，
/// 「裡/裏/里」「着/著」这些实际会绊倒人的对子都不在里面。真正管用的是
/// 下面这三个。`kSpoofingVariant`（形近易混）不收 —— 那是安全用途，不是异体。
const VARIANT_FIELDS: [&str; 4] =
    ["kSemanticVariant", "kSimplifiedVariant", "kTraditionalVariant", "kZVariant"];

pub struct Variants(pub HashMap<char, Vec<char>>);

fn parse_codepoint(s: &str) -> Option<char> {
    // `U+439B<kMatthews` 这种带出处注解的要先截断
    let s = s.split('<').next()?;
    let hex = s.strip_prefix("U+")?;
    char::from_u32(u32::from_str_radix(hex, 16).ok()?)
}

impl Variants {
    pub fn load(path: &Path) -> Result<Self> {
        let mut m: HashMap<char, Vec<char>> = HashMap::new();
        for line in lines_of(path)? {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut it = line.split('\t');
            let (Some(cp), Some(field), Some(vals)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            if !VARIANT_FIELDS.contains(&field) {
                continue;
            }
            let Some(c) = parse_codepoint(cp) else { continue };
            // 这些字段常把字本身也列进去（U+91CC kSimplifiedVariant U+91CC），要滤掉
            let list: Vec<char> =
                vals.split_whitespace().filter_map(parse_codepoint).filter(|&v| v != c).collect();
            if !list.is_empty() {
                m.entry(c).or_default().extend(list);
            }
        }
        for v in m.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        Ok(Variants(m))
    }

    /// 生成一个词的异体写法。
    ///
    /// 只替换单个字符（组合爆炸不值得），并限制词长和产出数量。
    pub fn spellings(&self, word: &str, max_out: usize) -> Vec<String> {
        let chars: Vec<char> = word.chars().collect();
        if chars.len() > 4 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for i in 0..chars.len() {
            let Some(alts) = self.0.get(&chars[i]) else { continue };
            for &a in alts {
                if out.len() >= max_out {
                    return out;
                }
                let mut c2 = chars.clone();
                c2[i] = a;
                let s: String = c2.into_iter().collect();
                if s != word {
                    out.push(s);
                }
            }
        }
        out
    }
}

// ─────────────────────────── 例句 ───────────────────────────

/// Tatoeba 的中英对照句对。
pub struct Pairs {
    /// 按句子长度升序 —— 短句先分配，词条拿到的例句更简洁。
    pub sorted: Vec<(String, String)>,
}

impl Pairs {
    pub fn load(cmn: &Path, eng: &Path, links: &Path) -> Result<Self> {
        // 1. 中文句：id → 文本
        let mut cmn_text: HashMap<u32, String> = HashMap::with_capacity(120_000);
        for line in lines_of(cmn)? {
            let mut it = line.splitn(3, '\t');
            let (Some(id), Some(_lang), Some(txt)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            if let Ok(id) = id.parse::<u32>() {
                cmn_text.insert(id, txt.to_string());
            }
        }

        // 2. 只扫一遍 links，挑出「中文句 → 英文句」的连边，并记下需要的英文 id
        let mut need_eng: HashSet<u32> = HashSet::with_capacity(200_000);
        let mut edges: Vec<(u32, u32)> = Vec::with_capacity(200_000);
        for line in lines_of(links)? {
            let mut it = line.split('\t');
            let (Some(a), Some(b)) = (it.next(), it.next()) else { continue };
            let (Ok(a), Ok(b)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>()) else {
                continue;
            };
            if cmn_text.contains_key(&a) {
                need_eng.insert(b);
                edges.push((a, b));
            }
        }

        // 3. 只留下真正被引用到的英文句，避免把 190 万句全读进内存
        let mut eng_text: HashMap<u32, String> = HashMap::with_capacity(need_eng.len());
        for line in lines_of(eng)? {
            let mut it = line.splitn(3, '\t');
            let (Some(id), Some(_lang), Some(txt)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let Ok(id) = id.parse::<u32>() else { continue };
            if need_eng.contains(&id) {
                eng_text.insert(id, txt.to_string());
            }
        }

        let mut seen: HashSet<u32> = HashSet::new();
        let mut sorted: Vec<(String, String)> = Vec::new();
        for (a, b) in edges {
            let (Some(zh), Some(en)) = (cmn_text.get(&a), eng_text.get(&b)) else { continue };
            // 一句中文只取第一条英译，避免同义重复挤占例句位
            if !seen.insert(a) {
                continue;
            }
            sorted.push((zh.clone(), en.clone()));
        }
        sorted.sort_by_key(|(zh, _)| zh.chars().count());
        Ok(Pairs { sorted })
    }
}

/// 正向最大匹配：在句子里找出所有属于词表的词。
///
/// 例句挂载不需要完美分词，宁可多挂几个词头。
pub fn zh_words_in(sentence: &str, vocab: &HashSet<String>, max_word: usize) -> Vec<String> {
    let chars: Vec<char> = sentence.chars().collect();
    let mut out = Vec::new();
    for i in 0..chars.len() {
        let hi = (i + max_word).min(chars.len());
        // 由长到短试，命中最长的那个就够了
        for j in (i + 1..=hi).rev() {
            let w: String = chars[i..j].iter().collect();
            if vocab.contains(&w) {
                out.push(w);
                break;
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 英文句切词，小写化。
pub fn en_words_in(sentence: &str) -> Vec<String> {
    let mut out: Vec<String> = sentence
        .split(|c: char| !c.is_ascii_alphabetic() && c != '\'')
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_lowercase())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codepoints_with_annotations_parse() {
        assert_eq!(parse_codepoint("U+4E2D"), Some('中'));
        assert_eq!(parse_codepoint("U+439B<kMatthews"), char::from_u32(0x439B));
        assert_eq!(parse_codepoint("garbage"), None);
    }

    #[test]
    fn variant_spellings_swap_one_character() {
        let mut m = HashMap::new();
        m.insert('里', vec!['裏', '裡']);
        let v = Variants(m);
        let mut got = v.spellings("里面", 8);
        got.sort();
        assert_eq!(got, vec!["裏面", "裡面"]);
        // 过长的词不展开
        assert!(v.spellings("里面里面里", 8).is_empty());
    }

    #[test]
    fn longest_match_finds_headwords() {
        let vocab: HashSet<String> = ["中", "中国", "人"].iter().map(|s| s.to_string()).collect();
        let got = zh_words_in("中国人", &vocab, 4);
        // 最大匹配吃掉「中国」，剩下「人」
        assert_eq!(got, vec!["中国", "人"]);
    }

    #[test]
    fn english_tokenizer_lowercases_and_dedupes() {
        assert_eq!(en_words_in("The cat, the CAT!"), vec!["cat", "the"]);
    }
}
