//! 构建期：把 CC-CEDICT / ECDICT / Tatoeba / Unihan / jieba 归一成一套
//! mmap 只读词库 + 多路 FST 前缀索引。跑一次，之后运行期纯查表。
//!
//! ```text
//! cargo run --release -p dict-build -- [--data data/raw] [--out data/index] [--max-en N]
//! ```

mod cedict;
mod corpus;
mod english;
mod fetch;
mod llm;
mod sqlite;

use anyhow::{Context, Result};
use corpus::{Pairs, Variants, ZhFreq};
use dict_core::model::Example;
use dict_core::store::{pack_top, unpack_top, MAX_TOP_PREFIX};
use dict_core::{percentiles, Lane, Params, StoreWriter, LANES};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// 每条词头最多挂几条例句。
const MAX_EXAMPLES: usize = 3;
/// 中文正向最大匹配的最长词长。
const MAX_ZH_WORD: usize = 6;

struct Args {
    data: PathBuf,
    out: PathBuf,
    max_en: usize,
}

fn parse_args() -> Args {
    let mut a = Args {
        data: PathBuf::from("data/raw"),
        out: PathBuf::from("data/index"),
        max_en: usize::MAX,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--data" if i + 1 < argv.len() => {
                a.data = PathBuf::from(&argv[i + 1]);
                i += 2;
            }
            "--out" if i + 1 < argv.len() => {
                a.out = PathBuf::from(&argv[i + 1]);
                i += 2;
            }
            "--max-en" if i + 1 < argv.len() => {
                a.max_en = argv[i + 1].parse().unwrap_or(usize::MAX);
                i += 2;
            }
            other => {
                eprintln!("忽略无法识别的参数: {other}");
                i += 1;
            }
        }
    }
    a
}

/// 累积某个通道的键 → 词条 id。
#[derive(Default)]
struct LaneKeys(HashMap<String, Vec<u32>>);

impl LaneKeys {
    fn add(&mut self, key: &str, id: u32) {
        if key.is_empty() {
            return;
        }
        self.0.entry(key.to_string()).or_default().push(id);
    }

    /// fst 要求键按字典序且唯一。
    fn finish(self) -> Vec<(String, Vec<u32>)> {
        let mut v: Vec<(String, Vec<u32>)> = self
            .0
            .into_iter()
            .map(|(k, mut ids)| {
                ids.sort_unstable();
                ids.dedup();
                (k, ids)
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

fn step(label: &str, t: &Instant) {
    println!("  [{:>6.1}s] {label}", t.elapsed().as_secs_f32());
}

/// 短前缀预算表：一个前缀下保留多少条候选。
const TOP_N: usize = 400;
/// 前缀底下的键数超过这个才值得建表 —— 少于此数直接流式扫描更快也更准。
const TOP_THRESHOLD: usize = 1500;

fn prefix_of(s: &str, n: usize) -> Option<String> {
    let mut it = s.chars();
    let p: String = it.by_ref().take(n).collect();
    (p.chars().count() == n).then_some(p)
}

/// 为扫描代价高的短前缀预算出前 `TOP_N` 名。
///
/// 前缀 `a` 在英文侧要流过九万个键，fst 逐键重建键字节约需 25 ms —— 这是每次
/// 查英文单词的第一次击键都要付的代价。这里把结果预先算好。
///
/// 排名用 `Params::default()` 打分。界面上的滑块调参后名次可能与真实排序略有
/// 出入，但 400 的深度远超 8 条候选的需要，实际看不出来。
fn build_top_index(
    lane: Lane,
    keys: &[(String, Vec<u32>)],
    pct: &[f32],
) -> Vec<(String, Vec<u32>)> {
    let p = Params::default();

    let mut count: HashMap<String, usize> = HashMap::new();
    for (k, _) in keys {
        for n in 1..=MAX_TOP_PREFIX {
            let Some(pre) = prefix_of(k, n) else { break };
            *count.entry(pre).or_default() += 1;
        }
    }
    count.retain(|_, c| *c > TOP_THRESHOLD);
    if count.is_empty() {
        return Vec::new();
    }

    let mut cand: HashMap<String, Vec<(f32, u32)>> = HashMap::with_capacity(count.len());
    for (k, ids) in keys {
        let klen = k.chars().count();
        for n in 1..=MAX_TOP_PREFIX {
            let Some(pre) = prefix_of(k, n) else { break };
            if !count.contains_key(&pre) {
                continue;
            }
            let slot = cand.entry(pre).or_default();
            for &id in ids {
                let v = pct.get(id as usize).copied().unwrap_or(0.0);
                slot.push((p.score(lane, klen, n, v), pack_top(klen, id)));
            }
        }
    }

    let mut out: Vec<(String, Vec<u32>)> = cand
        .into_iter()
        .map(|(pre, mut v)| {
            v.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            // 同一个 id 可能经由多个键落进同一前缀，只留分最高的那次
            let mut seen = std::collections::HashSet::new();
            v.retain(|(_, packed)| seen.insert(unpack_top(*packed).1));
            v.truncate(TOP_N);
            (pre, v.into_iter().map(|(_, packed)| packed).collect())
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn main() -> Result<()> {
    // `dict-build llm …` 是义项重组的子命令，跟主 ETL 完全分开
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("llm") {
        return llm::main(&argv[1..]);
    }
    // `dict-build fetch` 把数据源拉下来解开。发布包里的用户没有 bash，
    // 这一步得由程序自己做，见 fetch 模块。
    if argv.first().map(String::as_str) == Some("fetch") {
        return fetch::main(&argv[1..]);
    }

    let args = parse_args();
    let t = Instant::now();
    println!("构建词库：{} → {}", args.data.display(), args.out.display());

    // ── 中文侧 ────────────────────────────────────────────────
    let freq = ZhFreq::load(&args.data.join("jieba_dict.txt"))?;
    step(&format!("jieba 词频 {} 词", freq.count.len()), &t);

    let cedict_path = args.data.join("cedict.txt");
    let file = File::open(&cedict_path).with_context(|| {
        format!("打不开 {}（需要先把 cedict.txt.gz 解开）", cedict_path.display())
    })?;
    let mut zh: Vec<cedict::RawZh> = Vec::with_capacity(130_000);
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if let Some(mut r) = cedict::parse_line(&line) {
            r.entry.freq = freq.count.get(&r.entry.word).copied().unwrap_or(0.0) as f32;
            if let Some(p) = freq.pos.get(&r.entry.word) {
                r.entry.pos = p.clone();
            }
            zh.push(r);
        }
    }
    step(&format!("CC-CEDICT {} 条词头", zh.len()), &t);

    let zh_freqs: Vec<Option<f64>> = zh
        .iter()
        .map(|r| if r.entry.freq > 0.0 { Some(r.entry.freq as f64) } else { None })
        .collect();
    for (r, p) in zh.iter_mut().zip(percentiles(&zh_freqs, true)) {
        r.entry.pct = p;
    }

    // ── 英文侧 ────────────────────────────────────────────────
    let db_path = args.data.join("ecdict/stardict.db");
    let db = sqlite::Sqlite::open(db_path.to_str().context("ECDICT 路径不是合法 UTF-8")?)?;
    let total = db.scalar_i64("SELECT COUNT(*) FROM stardict")?;
    step(&format!("ECDICT 共 {total} 行"), &t);

    let mut en: Vec<english::RawEn> = Vec::with_capacity(1 << 20);
    let mut skipped = 0usize;
    db.each_row(
        "SELECT word, phonetic, definition, translation, pos, collins, oxford, tag, bnc, frq, exchange \
         FROM stardict",
        |row| {
            let r = english::EcRow {
                word: &row[0],
                phonetic: &row[1],
                definition: &row[2],
                translation: &row[3],
                pos: &row[4],
                collins: &row[5],
                oxford: &row[6],
                tag: &row[7],
                bnc: &row[8],
                frq: &row[9],
                exchange: &row[10],
            };
            match english::build(&r) {
                Some(x) if keep_en(&x) => en.push(x),
                _ => skipped += 1,
            }
            en.len() < args.max_en
        },
    )?;
    step(&format!("ECDICT 保留 {} 条，滤除 {}", en.len(), skipped), &t);

    let en_ranks: Vec<Option<f64>> = en.iter().map(|r| r.rank).collect();
    for (r, p) in en.iter_mut().zip(percentiles(&en_ranks, false)) {
        r.entry.pct = p;
    }

    // ── 例句 ──────────────────────────────────────────────────
    attach_examples(&args.data, &mut zh, &mut en, &t)?;

    // ── 落库 ──────────────────────────────────────────────────
    let variants = Variants::load(&args.data.join("unihan/Unihan_Variants.txt"))?;
    step(&format!("Unihan 异体字 {} 组", variants.0.len()), &t);

    std::fs::create_dir_all(&args.out)?;
    let mut w = StoreWriter::create(&args.out)?;
    let mut keys: HashMap<usize, LaneKeys> =
        LANES.iter().map(|l| (l.idx(), LaneKeys::default())).collect();

    let mut all_pct: Vec<f32> = Vec::with_capacity(zh.len() + en.len());
    for r in &zh {
        let id = w.push(&r.entry)?;
        all_pct.push(r.entry.pct);
        let e = &r.entry;
        let zh_lane = keys.get_mut(&Lane::Zh.idx()).unwrap();
        zh_lane.add(&e.word.to_lowercase(), id);
        if !e.trad.is_empty() {
            zh_lane.add(&e.trad.to_lowercase(), id);
        }
        // 异体字归一：不做的话查「著/着」「裡/里」会漏
        for v in variants.spellings(&e.word, 6) {
            zh_lane.add(&v.to_lowercase(), id);
        }

        // 子串索引 = 词头的全部真后缀，前缀查这张表即得「包含」语义
        let sub = keys.get_mut(&Lane::ZhSub.idx()).unwrap();
        let chars: Vec<char> = e.word.chars().collect();
        for s in 1..chars.len() {
            sub.add(&chars[s..].iter().collect::<String>().to_lowercase(), id);
        }

        let py = keys.get_mut(&Lane::Py.idx()).unwrap();
        for k in &r.pinyin_keys {
            py.add(k, id);
        }
        keys.get_mut(&Lane::Ini.idx()).unwrap().add(&r.initials, id);
        let gl = keys.get_mut(&Lane::Gloss.idx()).unwrap();
        for tkn in &r.gloss_tokens {
            gl.add(tkn, id);
        }
    }
    step(&format!("写入中文侧 {} 条", w.len()), &t);

    // 英文词形还原要用「拼写 → 原型 id」，所以先把原型的 id 全部登记下来
    let en_base = w.len() as u32;
    let mut en_id_of: HashMap<String, u32> = HashMap::with_capacity(en.len());
    for (i, r) in en.iter().enumerate() {
        en_id_of.insert(r.entry.word.to_lowercase(), en_base + i as u32);
    }

    for r in &en {
        let id = w.push(&r.entry)?;
        all_pct.push(r.entry.pct);
        let enw = keys.get_mut(&Lane::Enw.idx()).unwrap();
        let lower = r.entry.word.to_lowercase();
        enw.add(&lower, id);
        // 输入 `compiled` 要能回到 `compile`：把屈折形也指向原型词条
        if let Some(lemma) = &r.lemma {
            if let Some(&lid) = en_id_of.get(lemma) {
                if lid != id {
                    enw.add(&lower, lid);
                }
            }
        }
        let tr = keys.get_mut(&Lane::Trans.idx()).unwrap();
        for tkn in &r.trans_tokens {
            tr.add(tkn, id);
        }
    }
    step(&format!("写入英文侧，合计 {} 条", w.len()), &t);

    for lane in LANES {
        let k = keys.remove(&lane.idx()).unwrap_or_default().finish();
        w.write_lane(lane, &k)?;
        let tops = build_top_index(lane, &k, &all_pct);
        if !tops.is_empty() {
            w.write_top_lane(lane, &tops)?;
        }
        println!(
            "    通道 {:<6} {:>9} 键   预算表 {:>5} 前缀",
            lane.file_stem(),
            k.len(),
            tops.len()
        );
    }
    let n = w.len();
    w.finish()?;
    step(&format!("完成，共 {n} 条词头"), &t);
    Ok(())
}

/// ECDICT 有 340 万行，大量是无频次的专名和长短语。
///
/// 保留标准：有中文释义，且（词频已知 ∨ 有考试/词典标记 ∨ 是一个普通单词）。
fn keep_en(r: &english::RawEn) -> bool {
    if r.rank.is_some() || !r.entry.tags.is_empty() {
        return true;
    }
    let w = r.entry.word.as_str();
    let single = !w.contains(' ') && w.len() >= 2 && w.len() <= 24;
    single && w.chars().all(|c| c.is_ascii_alphabetic() || c == '-' || c == '\'')
}

/// 给词条挂 Tatoeba 例句。
///
/// 句对按长度升序处理，先到先得 —— 短句更适合当词条例句。
fn attach_examples(
    data: &Path,
    zh: &mut [cedict::RawZh],
    en: &mut [english::RawEn],
    t: &Instant,
) -> Result<()> {
    let pairs = Pairs::load(
        &data.join("cmn_sent.tsv"),
        &data.join("eng_sent.tsv"),
        &data.join("links.csv"),
    )?;
    step(&format!("Tatoeba 中英句对 {}", pairs.sorted.len()), t);

    // 一个词形可能对应多条词头（不同读音），例句挂给其中最常用的那条
    let mut zh_slot: HashMap<String, usize> = HashMap::with_capacity(zh.len());
    for (i, r) in zh.iter().enumerate() {
        zh_slot
            .entry(r.entry.word.clone())
            .and_modify(|best| {
                if zh[*best].entry.pct < r.entry.pct {
                    *best = i;
                }
            })
            .or_insert(i);
    }
    let zh_vocab: std::collections::HashSet<String> = zh_slot.keys().cloned().collect();

    let mut en_slot: HashMap<String, usize> = HashMap::with_capacity(en.len());
    for (i, r) in en.iter().enumerate() {
        en_slot.entry(r.entry.word.to_lowercase()).or_insert(i);
    }

    let (mut nzh, mut nen) = (0usize, 0usize);
    for (zh_text, en_text) in &pairs.sorted {
        for w in corpus::zh_words_in(zh_text, &zh_vocab, MAX_ZH_WORD) {
            let Some(&i) = zh_slot.get(&w) else { continue };
            if zh[i].entry.examples.len() < MAX_EXAMPLES {
                zh[i].entry.examples.push(Example { a: zh_text.clone(), b: en_text.clone() });
                nzh += 1;
            }
        }
        for w in corpus::en_words_in(en_text) {
            let Some(&i) = en_slot.get(&w) else { continue };
            if en[i].entry.examples.len() < MAX_EXAMPLES {
                en[i].entry.examples.push(Example { a: en_text.clone(), b: zh_text.clone() });
                nen += 1;
            }
        }
    }
    step(&format!("挂上例句：中文侧 {nzh} 条，英文侧 {nen} 条"), t);
    Ok(())
}
