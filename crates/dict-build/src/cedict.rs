//! CC-CEDICT 解析。
//!
//! 行格式：`繁體 简体 [pin1 yin1] /义项1/义项2/`
//!
//! 注音一律取 CC-CEDICT 的**词级**标注 —— 「银行 yin2 hang2」这类多音字
//! 只有词级标注是对的，字级映射拼出来会是 yin2 xing2。

use dict_core::model::{Sense, KIND_ZH};
use dict_core::pinyin::Reading;
use dict_core::Entry;

/// 一条尚未定频的中文词头。
pub struct RawZh {
    pub entry: Entry,
    /// 用于 `py` 通道的无调拼音（含 ü→v 变体，可能与第一个相同）。
    pub pinyin_keys: Vec<String>,
    /// 用于 `ini` 通道的首字母串。
    pub initials: String,
    /// 用于 `gloss` 通道的英文释义词元。
    pub gloss_tokens: Vec<String>,
}

/// CC-CEDICT 用括号打的语域/领域标记。出现在义项开头时提出来单独放。
fn split_leading_parens(s: &str) -> (String, String) {
    let t = s.trim();
    if !t.starts_with('(') {
        return (String::new(), t.to_string());
    }
    let mut depth = 0usize;
    for (i, c) in t.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let reg = t[1..i].trim().to_string();
                    let rest = t[i + 1..].trim().to_string();
                    // 整条义项就是一个括号时不拆，否则正文会空掉
                    return if rest.is_empty() {
                        (String::new(), t.to_string())
                    } else {
                        (reg, rest)
                    };
                }
            }
            _ => {}
        }
    }
    (String::new(), t.to_string())
}

/// 把义项里的交叉引用 `繁|简[pin1 yin1]` / `词[pin1 yin1]` 摘出来，
/// 同时把正文清成人能读的形式（去掉注音方括号和繁体前缀）。
fn extract_refs(s: &str, xrefs: &mut Vec<String>) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '[' {
            // 找配对的 ]，整段注音丢弃
            if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == ']') {
                // 方括号前紧邻的那串 CJK 就是被引用的词
                let head: String =
                    out.chars().rev().take_while(|c| dict_core::is_cjk(*c)).collect();
                let head: String = head.chars().rev().collect();
                if !head.is_empty() {
                    xrefs.push(head);
                }
                i = end + 1;
                continue;
            }
        }
        if chars[i] == '|' {
            // `繁體|简体` —— 只留简体，把已写出的繁体退掉
            let trad_len = out.chars().rev().take_while(|c| dict_core::is_cjk(*c)).count();
            for _ in 0..trad_len {
                out.pop();
            }
            i += 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out.trim().to_string()
}

/// 剥掉注音方括号之后只剩空壳的义项。
///
/// CC-CEDICT 里 `also pr. [hua1]`、`Taiwan pr. [bi3]` 这类条目，唯一的内容就在
/// 方括号里；括号一去就什么都不剩，留在释义里是纯噪音（全库 685 条）。
///
/// 注意 `variant of 裏|里[li3]` **不在此列** —— 它剥完还剩「里」，是有效的交叉引用。
/// 判据是「剥完还剩不剩东西」，不是「开头像不像注记」。
fn is_degenerate(text: &str) -> bool {
    let t = text
        .trim()
        .trim_end_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
        .trim();
    if t.is_empty() {
        return true;
    }
    // 这些短语本身不携带语义，后面跟的内容才是；后面空了就是空壳。
    const STUBS: &[&str] = &[
        "also pr",
        "also pr. or",
        "also pr or",
        "taiwan pr",
        "also written",
        "also known as",
        "also translated",
        "erhua variant of",
        "abbr. for",
        "abbr for",
        "see",
        "see also",
        "variant of",
        "old variant of",
    ];
    let lower = t.to_lowercase();
    let lower = lower.trim_end_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
    STUBS.contains(&lower)
}

/// 从英文释义里切出用于 `gloss` 通道的词元。
fn tokenize_gloss(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric() && c != '\'')
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_lowercase())
        .collect()
}

/// 解析一行 CC-CEDICT。注释行和格式不符的行返回 `None`。
pub fn parse_line(line: &str) -> Option<RawZh> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let lb = line.find('[')?;
    let rb = line[lb..].find(']')? + lb;
    let head = line[..lb].trim();
    let mut parts = head.split_whitespace();
    let trad = parts.next()?.to_string();
    let simp = parts.next()?.to_string();
    let pinyin_raw = &line[lb + 1..rb];

    let body = line[rb + 1..].trim();
    let glosses: Vec<&str> = body.split('/').map(str::trim).filter(|s| !s.is_empty()).collect();
    if glosses.is_empty() {
        return None;
    }

    let reading = Reading::parse(pinyin_raw);
    let mut xrefs: Vec<String> = Vec::new();
    let mut classifiers: Vec<String> = Vec::new();
    let mut senses: Vec<Sense> = Vec::new();
    let mut gloss_tokens: Vec<String> = Vec::new();

    for g in &glosses {
        // 量词单独成行，不占义项号
        if let Some(cl) = g.strip_prefix("CL:") {
            let cleaned = extract_refs(cl, &mut Vec::new());
            classifiers.push(cleaned);
            continue;
        }
        let (reg, rest) = split_leading_parens(g);
        let text = extract_refs(&rest, &mut xrefs);
        if text.is_empty() {
            continue;
        }
        if is_degenerate(&text) {
            continue;
        }
        gloss_tokens.extend(tokenize_gloss(&text));
        senses.push(Sense { pos: String::new(), text, note: String::new(), reg });
    }
    if senses.is_empty() {
        // 整条都被判成空壳时宁可原样保留 —— 丢掉词条会让后面所有词条的 id 平移，
        // 而义项重组的旁路表是按 id 索引的。
        for g in &glosses {
            let (reg, rest) = split_leading_parens(g);
            let text = extract_refs(&rest, &mut Vec::new());
            if !text.is_empty() {
                senses.push(Sense { pos: String::new(), text, note: String::new(), reg });
            }
        }
        if senses.is_empty() {
            return None;
        }
    }

    xrefs.sort();
    xrefs.dedup();
    xrefs.retain(|x| x != &simp && x != &trad);
    xrefs.truncate(8);
    gloss_tokens.sort();
    gloss_tokens.dedup();

    let mut pinyin_keys = vec![reading.plain()];
    let pv = reading.plain_v();
    if pv != pinyin_keys[0] {
        pinyin_keys.push(pv);
    }
    pinyin_keys.retain(|k| !k.is_empty());

    let entry = Entry {
        kind: KIND_ZH,
        word: simp.clone(),
        trad: if trad == simp { String::new() } else { trad },
        reading: reading.toned(),
        tones: reading.tones(),
        freq: 0.0,
        pct: 0.0,
        pos: String::new(),
        forms: if classifiers.is_empty() {
            String::new()
        } else {
            format!("量词 {}", classifiers.join(" · "))
        },
        tags: String::new(),
        senses,
        examples: Vec::new(),
        raw: body.trim_matches('/').replace('/', " / "),
        xrefs,
    };

    Some(RawZh { entry, pinyin_keys, initials: reading.initials(), gloss_tokens })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_entry() {
        let r = parse_line("中國 中国 [Zhong1 guo2] /China/Middle Kingdom/").unwrap();
        assert_eq!(r.entry.word, "中国");
        assert_eq!(r.entry.trad, "中國");
        assert_eq!(r.entry.reading, "Zh\u{14d}ng gu\u{f3}");
        assert_eq!(r.entry.senses.len(), 2);
        assert_eq!(r.entry.senses[0].text, "China");
        assert_eq!(r.pinyin_keys[0], "zhongguo");
        assert_eq!(r.initials, "zg");
    }

    #[test]
    fn trad_equal_simp_leaves_trad_empty() {
        let r = parse_line("慢 慢 [man4] /slow/").unwrap();
        assert_eq!(r.entry.trad, "");
        assert_eq!(r.entry.reading, "m\u{e0}n");
    }

    #[test]
    fn pulls_register_marker_out_of_the_gloss() {
        let r = parse_line("將就 将就 [jiang1 jiu5] /(coll.) to make do/").unwrap();
        assert_eq!(r.entry.senses[0].reg, "coll.");
        assert_eq!(r.entry.senses[0].text, "to make do");
    }

    #[test]
    fn keeps_a_gloss_that_is_entirely_parenthetical() {
        let r = parse_line("阿 阿 [a1] /(prefix used before monosyllabic names)/").unwrap();
        assert_eq!(r.entry.senses[0].text, "(prefix used before monosyllabic names)");
    }

    #[test]
    fn extracts_xrefs_and_cleans_the_text() {
        let r = parse_line("裡 里 [li3] /variant of 裏|里[li3]/").unwrap();
        assert_eq!(r.entry.senses[0].text, "variant of 里");
        // 引用的是自己，应被过滤掉
        assert!(r.entry.xrefs.is_empty(), "xrefs = {:?}", r.entry.xrefs);

        let r = parse_line("甲 甲 [jia3] /see 乙丙[yi3 bing3]/").unwrap();
        assert_eq!(r.entry.senses[0].text, "see 乙丙");
        assert_eq!(r.entry.xrefs, vec!["乙丙"]);
    }

    #[test]
    fn classifiers_go_to_the_forms_line_not_a_sense() {
        let r = parse_line("書 书 [shu1] /book/CL:本[ben3],冊|册[ce4]/").unwrap();
        assert_eq!(r.entry.senses.len(), 1);
        assert!(r.entry.forms.starts_with("量词"), "forms = {:?}", r.entry.forms);
        assert!(r.entry.forms.contains('本'));
    }

    #[test]
    fn drops_senses_that_are_only_a_pronunciation_note() {
        // 唯一的内容在方括号里，剥完什么都不剩
        let r = parse_line("花 花 [hua1] /flower/also pr. [hua4]/").unwrap();
        assert_eq!(r.entry.senses.len(), 1, "senses = {:?}", r.entry.senses);
        assert_eq!(r.entry.senses[0].text, "flower");

        let r = parse_line("比 比 [bi3] /to compare/Taiwan pr. [bi4]/").unwrap();
        assert_eq!(r.entry.senses.len(), 1);
    }

    #[test]
    fn keeps_cross_references_that_still_have_content() {
        // 剥完还剩「里」，是有效的交叉引用，不能跟着一起滤掉
        let r = parse_line("裡 里 [li3] /variant of 裏|里[li3]/").unwrap();
        assert_eq!(r.entry.senses.len(), 1);
        assert_eq!(r.entry.senses[0].text, "variant of 里");
    }

    #[test]
    fn an_all_degenerate_entry_is_kept_rather_than_dropped() {
        // 丢词条会让后面所有 id 平移，而义项重组的旁路表按 id 索引 —— 绝不能丢
        let r = parse_line("啊 啊 [a1] /also pr. [a5]/").expect("整条都是空壳时也要保留");
        assert_eq!(r.entry.word, "啊");
        assert_eq!(r.entry.senses.len(), 1);
    }

    #[test]
    fn skips_comments_and_junk() {
        assert!(parse_line("# comment").is_none());
        assert!(parse_line("").is_none());
        assert!(parse_line("no brackets here").is_none());
    }

    #[test]
    fn umlaut_entry_gets_both_pinyin_spellings() {
        let r = parse_line("女 女 [nu:3] /female/").unwrap();
        assert_eq!(r.pinyin_keys, vec!["nu", "nv"]);
    }
}
