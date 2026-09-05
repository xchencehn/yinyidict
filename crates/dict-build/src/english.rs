//! ECDICT 行 → 英文词头。
//!
//! ECDICT 的结构化程度比 CC-CEDICT 好得多：`translation` 已经按行分好中文义项
//! 并带词性前缀，`definition` 有英文释义，`exchange` 有完整的时态派生。
//! 这里做的主要是把它们摆进统一的 `Entry`，并从 `exchange` 反推词形还原表。

use dict_core::model::{Sense, KIND_EN};
use dict_core::Entry;

/// ECDICT 一行的原始列。
pub struct EcRow<'a> {
    pub word: &'a str,
    pub phonetic: &'a str,
    pub definition: &'a str,
    pub translation: &'a str,
    pub pos: &'a str,
    pub collins: &'a str,
    pub oxford: &'a str,
    pub tag: &'a str,
    pub bnc: &'a str,
    pub frq: &'a str,
    pub exchange: &'a str,
}

pub struct RawEn {
    pub entry: Entry,
    /// BNC/COCA 排名，越小越常用；未知为 `None`。
    pub rank: Option<f64>,
    /// 中文释义里切出的词元，用于 `trans` 通道。
    pub trans_tokens: Vec<String>,
    /// `exchange` 里的 `0:` 原型。输入 `compiled` 要能回到 `compile`。
    pub lemma: Option<String>,
}

fn tag_label(t: &str) -> Option<&'static str> {
    Some(match t {
        "zk" => "中考",
        "gk" => "高考",
        "ky" => "考研",
        "cet4" => "四级",
        "cet6" => "六级",
        "toefl" => "托福",
        "ielts" => "雅思",
        "gre" => "GRE",
        _ => return None,
    })
}

fn exchange_label(k: &str) -> Option<&'static str> {
    Some(match k {
        "p" => "过去式",
        "d" => "过去分词",
        "i" => "现在分词",
        "3" => "三单",
        "r" => "比较级",
        "t" => "最高级",
        "s" => "复数",
        _ => return None,
    })
}

/// 拆 `d:constructed/p:constructed/3:constructs` 这样的 exchange 串。
fn parse_exchange(s: &str) -> (String, Option<String>) {
    let mut forms: Vec<String> = Vec::new();
    let mut lemma: Option<String> = None;
    for part in s.split('/') {
        let Some((k, v)) = part.split_once(':') else { continue };
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        if k == "0" {
            lemma = Some(v.to_lowercase());
        } else if let Some(label) = exchange_label(k) {
            forms.push(format!("{label} {v}"));
        }
    }
    (forms.join(" · "), lemma)
}

/// `n:50,v:30` → 取占比最大的词性。
fn dominant_pos(s: &str) -> String {
    s.split(',')
        .filter_map(|p| {
            let (k, v) = p.split_once(':')?;
            Some((k.trim().to_string(), v.trim().parse::<i32>().unwrap_or(0)))
        })
        .max_by_key(|(_, v)| *v)
        .map(|(k, _)| format!("{k}."))
        .unwrap_or_default()
}

/// ECDICT 的中文释义行常带词性前缀：`n. 编译器`。把前缀切下来。
fn split_pos_prefix(line: &str) -> (String, String) {
    let t = line.trim();
    if let Some((head, rest)) = t.split_once(char::is_whitespace) {
        let h = head.trim();
        let looks_like_pos = h.ends_with('.')
            && h.len() <= 8
            && h.chars().all(|c| c.is_ascii_alphabetic() || c == '.');
        let looks_like_domain = h.starts_with('[') && h.ends_with(']');
        if looks_like_pos || looks_like_domain {
            return (h.to_string(), rest.trim().to_string());
        }
    }
    (String::new(), t.to_string())
}

fn tokenize_cjk(s: &str) -> Vec<String> {
    // 中文释义按非汉字切段，段本身直接作为 `trans` 通道的键。
    // 用户输入「编」应能前缀命中「编译」，所以整段入索引即可。
    s.split(|c: char| !dict_core::is_cjk(c))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

pub fn build(row: &EcRow<'_>) -> Option<RawEn> {
    let word = row.word.trim();
    if word.is_empty() || row.translation.trim().is_empty() {
        return None;
    }

    let cn_lines: Vec<&str> =
        row.translation.split('\n').map(str::trim).filter(|s| !s.is_empty()).collect();
    let en_lines: Vec<&str> =
        row.definition.split('\n').map(str::trim).filter(|s| !s.is_empty()).collect();
    if cn_lines.is_empty() {
        return None;
    }

    // 义项以中文释义为主干；英文释义行数对得上时逐条配成补充说明。
    let pair_up = en_lines.len() == cn_lines.len();
    let mut senses = Vec::with_capacity(cn_lines.len());
    let mut trans_tokens = Vec::new();
    for (i, line) in cn_lines.iter().enumerate() {
        let (pos, text) = split_pos_prefix(line);
        trans_tokens.extend(tokenize_cjk(&text));
        senses.push(Sense {
            pos,
            text,
            note: if pair_up { en_lines[i].to_string() } else { String::new() },
            reg: String::new(),
        });
    }
    trans_tokens.sort();
    trans_tokens.dedup();

    let (forms, lemma) = parse_exchange(row.exchange);

    let mut tags: Vec<String> =
        row.tag.split_whitespace().filter_map(tag_label).map(str::to_string).collect();
    if let Ok(c) = row.collins.trim().parse::<i32>() {
        if c > 0 {
            tags.push(format!("柯林斯 {}", "★".repeat(c.min(5) as usize)));
        }
    }
    if row.oxford.trim() == "1" {
        tags.push("牛津核心".to_string());
    }

    let bnc = row.bnc.trim().parse::<f64>().ok().filter(|v| *v > 0.0);
    let frq = row.frq.trim().parse::<f64>().ok().filter(|v| *v > 0.0);
    let rank = match (bnc, frq) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };

    let phonetic = row.phonetic.trim();
    let mut raw = row.translation.trim().to_string();
    if !row.definition.trim().is_empty() {
        raw.push_str("\n— — —\n");
        raw.push_str(row.definition.trim());
    }

    let entry = Entry {
        kind: KIND_EN,
        word: word.to_string(),
        trad: String::new(),
        reading: if phonetic.is_empty() { String::new() } else { format!("/{phonetic}/") },
        tones: Vec::new(),
        freq: rank.unwrap_or(0.0) as f32,
        pct: 0.0,
        pos: dominant_pos(row.pos),
        forms,
        tags: tags.join(" · "),
        senses,
        examples: Vec::new(),
        raw,
        xrefs: Vec::new(),
    };

    Some(RawEn { entry, rank, trans_tokens, lemma })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row<'a>(word: &'a str, tr: &'a str, def: &'a str, ex: &'a str) -> EcRow<'a> {
        EcRow {
            word,
            phonetic: "kəm'paɪl",
            definition: def,
            translation: tr,
            pos: "v:80,n:20",
            collins: "3",
            oxford: "1",
            tag: "cet4 gre",
            bnc: "8500",
            frq: "9000",
            exchange: ex,
        }
    }

    #[test]
    fn builds_senses_from_chinese_lines() {
        let r = build(&row("compile", "vt. 编译\nvt. 编纂", "", "")).unwrap();
        assert_eq!(r.entry.senses.len(), 2);
        assert_eq!(r.entry.senses[0].pos, "vt.");
        assert_eq!(r.entry.senses[0].text, "编译");
        assert_eq!(r.entry.reading, "/kəm'paɪl/");
        assert_eq!(r.entry.pos, "v.");
    }

    #[test]
    fn pairs_english_definitions_when_line_counts_match() {
        let r =
            build(&row("compile", "vt. 编译\nvt. 编纂", "to translate source\nto assemble", ""))
                .unwrap();
        assert_eq!(r.entry.senses[0].note, "to translate source");
        assert_eq!(r.entry.senses[1].note, "to assemble");

        // 行数对不上就不硬配，免得张冠李戴
        let r = build(&row("compile", "vt. 编译\nvt. 编纂", "only one line", "")).unwrap();
        assert_eq!(r.entry.senses[0].note, "");
    }

    #[test]
    fn parses_exchange_into_a_forms_line_and_a_lemma() {
        let r = build(&row("compiled", "vt. 编译", "", "0:compile/1:p")).unwrap();
        assert_eq!(r.lemma.as_deref(), Some("compile"));

        let r =
            build(&row("compile", "vt. 编译", "", "p:compiled/i:compiling/3:compiles")).unwrap();
        assert!(r.entry.forms.contains("过去式 compiled"), "forms = {}", r.entry.forms);
        assert!(r.entry.forms.contains("三单 compiles"), "forms = {}", r.entry.forms);
        assert!(r.lemma.is_none());
    }

    #[test]
    fn rank_takes_the_more_common_of_bnc_and_coca() {
        let r = build(&row("compile", "vt. 编译", "", "")).unwrap();
        assert_eq!(r.rank, Some(8500.0));
    }

    #[test]
    fn exam_tags_and_collins_stars() {
        let r = build(&row("compile", "vt. 编译", "", "")).unwrap();
        assert!(r.entry.tags.contains("四级"));
        assert!(r.entry.tags.contains("GRE"));
        assert!(r.entry.tags.contains("★★★"));
        assert!(r.entry.tags.contains("牛津核心"));
    }

    #[test]
    fn chinese_gloss_tokens_feed_the_trans_lane() {
        let r = build(&row("compile", "vt. 编译；编纂", "", "")).unwrap();
        assert!(r.trans_tokens.contains(&"编译".to_string()));
        assert!(r.trans_tokens.contains(&"编纂".to_string()));
    }

    #[test]
    fn rejects_rows_without_a_chinese_translation() {
        assert!(build(&row("zzz", "", "", "")).is_none());
    }
}
