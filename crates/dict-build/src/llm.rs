//! 用 Claude 重组义项。
//!
//! **角色是「重组」不是「生成」。** 模型看到的只有 CC-CEDICT 已有的义项和挂好的
//! Tatoeba 例句，它做的是排序、合并、加语域和搭配标注。每个输出义项必须用 `from`
//! 指出它来自哪几条输入义项，这样才能机器校验，而不是靠信任。
//!
//! 拼音、词形、词频一律不进出模型 —— 尤其是声调，模型编得极像真的。
//!
//! 结果落在旁路表 `llm.bin`/`llm.idx`，不动主索引：重跑 ETL 不会丢掉这份产出，
//! 换模型或改 prompt 也能单独重来。运行期有就覆盖显示，没有就回退原始释义。
//!
//! ```text
//! dict-build llm export                     # 选出目标词条，写 work.jsonl
//! dict-build llm run --jobs 6 --batch 25    # 调 claude 重组，可断点续跑
//! dict-build llm import                     # 合进旁路表
//! ```

use anyhow::{bail, Context, Result};
use dict_core::{Entry, Store};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// 义项数少于这个的词条不送 —— CC-CEDICT 里六成词条只有一个义项，
/// 让模型碰它们只增加幻觉风险，没有收益。
const MIN_SENSES: usize = 3;
/// 百分位低于这个的不送。低频词查得少，不值得花钱，也回退得起。
const MIN_PCT: f32 = 0.55;

// ─────────────────────────── 工作项 ───────────────────────────

/// 送进模型的接地材料。字段名全部压到最短 —— 这些 token 要乘以八千条。
struct Task {
    id: u32,
    word: String,
    pos: String,
    /// CC-CEDICT 原始义项，下标即 `from` 里引用的编号。
    src: Vec<String>,
    /// 最多两条例句，只给中文那半 —— 英文那半对重组英文释义没帮助。
    ex: Vec<String>,
}

/// 极简 JSON 字符串转义。工作文件是我们自己写自己读，不需要完整的 JSON 库。
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => {}
            '\t' => o.push(' '),
            c if (c as u32) < 0x20 => {}
            c => o.push(c),
        }
    }
    o
}

impl Task {
    fn to_json(&self) -> String {
        let arr = |v: &[String]| {
            v.iter().map(|s| format!("\"{}\"", esc(s))).collect::<Vec<_>>().join(",")
        };
        format!(
            "{{\"id\":{},\"w\":\"{}\",\"pos\":\"{}\",\"src\":[{}],\"ex\":[{}]}}",
            self.id,
            esc(&self.word),
            esc(&self.pos),
            arr(&self.src),
            arr(&self.ex)
        )
    }
}

fn build_task(id: u32, e: &Entry) -> Task {
    Task {
        id,
        word: e.word.clone(),
        pos: e.pos.clone(),
        src: e
            .senses
            .iter()
            .map(|s| {
                // 原文里的语域标记也一起给出去，模型才知道哪些已经标过了
                if s.reg.is_empty() {
                    s.text.clone()
                } else {
                    format!("({}) {}", s.reg, s.text)
                }
            })
            .collect(),
        ex: e.examples.iter().take(2).map(|x| x.a.clone()).collect(),
    }
}

// ─────────────────────────── export ───────────────────────────

fn export(index: &Path, out: &Path, limit: usize) -> Result<()> {
    let store = Store::open_raw(index)?;
    let mut n = 0usize;
    let mut f = std::io::BufWriter::new(std::fs::File::create(out)?);
    // 按百分位从高到低送，这样中途停下时先跑完的是最常查的词
    let mut cand: Vec<(u32, f32)> = (0..store.n as u32)
        .filter(|&i| store.kind[i as usize] == dict_core::KIND_ZH)
        .map(|i| (i, store.pct[i as usize]))
        .filter(|&(_, p)| p >= MIN_PCT)
        .collect();
    cand.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    for (id, _) in cand {
        let e = store.entry(id)?;
        if e.senses.len() < MIN_SENSES {
            continue;
        }
        writeln!(f, "{}", build_task(id, &e).to_json())?;
        n += 1;
        if n >= limit {
            break;
        }
    }
    f.flush()?;
    println!("导出 {n} 条 → {}", out.display());
    Ok(())
}

// ─────────────────────────── run ───────────────────────────

const SYSTEM: &str = "\
你是汉英词典的义项编辑。输入是若干条 CC-CEDICT 词条的原始英文释义，\
你的任务是【重新组织】它们，不是重写，更不是创作。

规则：
1. 只能使用输入里已有的语义。绝不引入输入中没有的义项。
2. 每个输出义项必须用 from 列出它来自哪几条输入义项的下标（从 0 开始）。
3. 按实际使用频率排序，最常用的排第一。
4. 语义相同或极近的输入义项合并成一条，用分号连接。
5. reg 填语域/领域，沿用 CC-CEDICT 的写法：coll./lit./fig./computing/medicine/\
law/dialect/archaic/derog./honorific 等。没有把握就填空字符串。
6. col 填搭配，只填能从例句或释义确认的，最多两个，用 / 分隔。没有就填空字符串。
7. 不要输出拼音、声调、词形、词频 —— 这些有权威数据，不归你管。
8. 输出义项数不得超过输入义项数。
9. **每条输入义项都必须被至少一个输出义项的 from 引用**。合并可以，丢弃不行 ——
   下标 0 到 n-1 一个都不能漏。

只输出一个 JSON 数组，每个元素形如
{\"id\":123,\"senses\":[{\"from\":[0,2],\"text\":\"...\",\"reg\":\"\",\"col\":\"\"}]}
不要 markdown 代码块，不要任何解释文字。";

/// 一条重组结果。
struct Out {
    id: u32,
    senses: Vec<OutSense>,
}
struct OutSense {
    from: Vec<usize>,
    text: String,
    reg: String,
    col: String,
}

fn run(work: &Path, out: &Path, jobs: usize, batch: usize, model: &str) -> Result<()> {
    let tasks: Vec<(u32, String)> = BufReader::new(std::fs::File::open(work)?)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| json_u32(&l, "id").map(|id| (id, l)))
        .collect();

    // 断点续跑：已经有结果的 id 直接跳过
    let done: HashSet<u32> = if out.exists() {
        BufReader::new(std::fs::File::open(out)?)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| json_u32(&l, "id"))
            .collect()
    } else {
        HashSet::new()
    };
    let todo: Vec<(u32, String)> = tasks.into_iter().filter(|(id, _)| !done.contains(id)).collect();
    if todo.is_empty() {
        println!("没有待处理的词条（已完成 {}）", done.len());
        return Ok(());
    }
    println!("待处理 {} 条（已完成 {}），{jobs} 路并发 × 每批 {batch} 条", todo.len(), done.len());

    let chunks: Vec<Vec<(u32, String)>> = todo.chunks(batch).map(|c| c.to_vec()).collect();
    let total = chunks.len();
    let queue = Arc::new(Mutex::new(chunks.into_iter().enumerate().collect::<Vec<_>>()));
    let sink =
        Arc::new(Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(out)?));
    let stats = Arc::new(Mutex::new((0usize, 0usize, 0usize))); // 完成批次 / 收到条数 / 丢弃条数
    let t0 = Instant::now();

    std::thread::scope(|s| {
        for _ in 0..jobs.max(1) {
            let queue = Arc::clone(&queue);
            let sink = Arc::clone(&sink);
            let stats = Arc::clone(&stats);
            let model = model.to_string();
            s.spawn(move || loop {
                let Some((i, chunk)) = queue.lock().ok().and_then(|mut q| q.pop()) else {
                    return;
                };
                let ids: HashSet<u32> = chunk.iter().map(|(id, _)| *id).collect();
                let payload: String =
                    chunk.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n");

                match call_claude(&payload, &model) {
                    Ok(reply) => {
                        let outs = parse_outs(&reply, &ids);
                        let kept = outs.len();
                        if let Ok(mut f) = sink.lock() {
                            for o in &outs {
                                let _ = writeln!(f, "{}", out_json(o));
                            }
                            let _ = f.flush();
                        }
                        if let Ok(mut st) = stats.lock() {
                            st.0 += 1;
                            st.1 += kept;
                            st.2 += ids.len() - kept;
                            let (b, k, d) = *st;
                            let per = t0.elapsed().as_secs_f64() / b.max(1) as f64;
                            println!(
                                "  [{b}/{total}] 批 {i} 收 {kept}/{}，累计 {k} 条（丢 {d}），\
                                 平均 {per:.0}s/批，预计还需 {:.0} 分钟",
                                ids.len(),
                                per * (total - b) as f64 / 60.0
                            );
                        }
                    }
                    Err(e) => eprintln!("  批 {i} 失败：{e:#}"),
                }
            });
        }
    });

    let (b, k, d) = *stats.lock().unwrap();
    println!(
        "完成 {b}/{total} 批，写入 {k} 条，丢弃 {d} 条，用时 {:.1} 分钟",
        t0.elapsed().as_secs_f64() / 60.0
    );
    Ok(())
}

/// 调一次 `claude -p`。
///
/// 关掉全部工具：这是纯文本改写，不该让它去读文件或搜网。
fn call_claude(payload: &str, model: &str) -> Result<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("claude")
        .args([
            "-p",
            "--model",
            model,
            "--system-prompt",
            SYSTEM,
            "--permission-mode",
            "default",
            "--disallowed-tools",
            "Bash,Read,Write,Edit,Glob,Grep,WebFetch,WebSearch,Task",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("启动 claude 失败（确认它在 PATH 上）")?;
    child.stdin.take().context("拿不到 claude 的 stdin")?.write_all(payload.as_bytes())?;
    let o = child.wait_with_output()?;
    if !o.status.success() {
        bail!("claude 退出码 {:?}: {}", o.status.code(), String::from_utf8_lossy(&o.stderr));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

// ─────────────────────────── 解析与校验 ───────────────────────────

/// 从一行 JSON 里抠出某个整数字段。工作文件格式是我们自己定的，够用。
fn json_u32(line: &str, key: &str) -> Option<u32> {
    let pat = format!("\"{key}\":");
    let i = line.find(&pat)? + pat.len();
    let rest = line[i..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// 把模型回复里的 JSON 数组解出来，并逐条校验。
///
/// 校验点：id 必须是这批送出去的；`from` 必须落在输入义项范围内；text 不能为空。
/// 不满足的直接丢掉 —— 宁可回退原始释义，也不要一条来路不明的释义。
fn parse_outs(reply: &str, ids: &HashSet<u32>) -> Vec<Out> {
    let body = strip_fence(reply);
    let mut out = Vec::new();
    for obj in split_objects(&body) {
        let Some(id) = json_u32(&obj, "id") else { continue };
        if !ids.contains(&id) {
            continue;
        }
        let senses = parse_senses(&obj);
        if senses.is_empty() {
            continue;
        }
        out.push(Out { id, senses });
    }
    out
}

/// 去掉可能包着的 markdown 代码围栏。
fn strip_fence(s: &str) -> String {
    let t = s.trim();
    let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```")).unwrap_or(t);
    t.trim_start().strip_suffix("```").unwrap_or(t).trim().to_string()
}

/// 按大括号配对切出顶层对象（字符串内的括号要跳过）。
fn split_objects(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut start, mut in_str, mut esc_next) = (0i32, 0usize, false, false);
    for (i, c) in s.char_indices() {
        if in_str {
            if esc_next {
                esc_next = false;
            } else if c == '\\' {
                esc_next = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    out.push(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    out
}

fn parse_senses(obj: &str) -> Vec<OutSense> {
    let Some(i) = obj.find("\"senses\"") else { return Vec::new() };
    let body = &obj[i..];
    split_objects(body)
        .into_iter()
        .filter_map(|s| {
            let text = json_str(&s, "text")?;
            if text.trim().is_empty() {
                return None;
            }
            Some(OutSense {
                from: json_usizes(&s, "from"),
                text,
                reg: json_str(&s, "reg").unwrap_or_default(),
                col: json_str(&s, "col").unwrap_or_default(),
            })
        })
        .collect()
}

fn json_str(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":");
    let i = s.find(&pat)? + pat.len();
    let rest = s[i..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut it = rest.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => return Some(out),
            '\\' => match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push(' '),
                Some(x) => out.push(x),
                None => return Some(out),
            },
            c => out.push(c),
        }
    }
    Some(out)
}

fn json_usizes(s: &str, key: &str) -> Vec<usize> {
    let pat = format!("\"{key}\":");
    let Some(i) = s.find(&pat) else { return Vec::new() };
    let rest = &s[i + pat.len()..];
    let Some(a) = rest.find('[') else { return Vec::new() };
    let Some(b) = rest[a..].find(']') else { return Vec::new() };
    rest[a + 1..a + b].split(',').filter_map(|t| t.trim().parse().ok()).collect()
}

fn out_json(o: &Out) -> String {
    let senses = o
        .senses
        .iter()
        .map(|s| {
            format!(
                "{{\"from\":[{}],\"text\":\"{}\",\"reg\":\"{}\",\"col\":\"{}\"}}",
                s.from.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","),
                esc(&s.text),
                esc(&s.reg),
                esc(&s.col)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"id\":{},\"senses\":[{}]}}", o.id, senses)
}

// ─────────────────────────── import ───────────────────────────

/// 把重组结果合进旁路表，并在这里做**接地校验**。
///
/// 这是最后一道闸：`from` 必须落在原始义项的下标范围内，义项数不能超过原始数。
/// 模型凭空造出来的义项拿不出合法的 `from`，会在这里被整条打回。
fn import(index: &Path, results: &Path) -> Result<()> {
    // 先把要用的东西全部读出来再放掉 store —— 它 mmap 着 llm.bin，
    // Windows 不允许改写一个还被映射着的文件（os error 1224）。
    let (n_entries, orig): (usize, std::collections::HashMap<u32, (usize, String)>) = {
        let store = Store::open_raw(index)?;
        let mut m = std::collections::HashMap::new();
        for line in BufReader::new(std::fs::File::open(results)?).lines().map_while(Result::ok) {
            let Some(id) = json_u32(&line, "id") else { continue };
            if let std::collections::hash_map::Entry::Vacant(v) = m.entry(id) {
                if let Ok(e) = store.entry(id) {
                    v.insert((e.senses.len(), e.pos.clone()));
                }
            }
        }
        (store.n, m)
    };

    let mut kept: Vec<(u32, Vec<dict_core::Sense>)> = Vec::new();
    let (mut total, mut bad_from, mut too_many, mut missing) = (0usize, 0usize, 0usize, 0usize);
    let mut uncovered = 0usize;

    for line in BufReader::new(std::fs::File::open(results)?).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let Some(id) = json_u32(&line, "id") else { continue };
        total += 1;
        let Some((n_src, pos)) = orig.get(&id) else {
            missing += 1;
            continue;
        };
        let senses = parse_senses(&line);
        if senses.is_empty() {
            missing += 1;
            continue;
        }
        if senses.len() > *n_src {
            too_many += 1;
            continue;
        }
        // 接地校验：模型凭空造的义项拿不出落在范围内的 from，整条打回
        if senses.iter().any(|s| s.from.is_empty() || s.from.iter().any(|&i| i >= *n_src)) {
            bad_from += 1;
            continue;
        }
        // 全覆盖：每条源义项都得被引用到。
        // 空壳义项（`also pr.` 那种）已经在 ETL 里滤掉了，所以到这一步
        // 「丢掉一条源义项」不再有正当理由 —— 那就是信息损失，整条打回。
        let covered: HashSet<usize> = senses.iter().flat_map(|s| s.from.iter().copied()).collect();
        if (0..*n_src).any(|i| !covered.contains(&i)) {
            uncovered += 1;
            continue;
        }
        kept.push((
            id,
            senses
                .into_iter()
                .map(|s| dict_core::Sense {
                    pos: pos.clone(),
                    text: s.text,
                    note: if s.col.trim().is_empty() {
                        String::new()
                    } else {
                        format!("搭配 {}", s.col.trim())
                    },
                    reg: s.reg,
                })
                .collect(),
        ));
    }

    kept.sort_by_key(|(id, _)| *id);
    kept.dedup_by_key(|(id, _)| *id);
    dict_core::StoreWriter::write_overlay(index, n_entries, &kept)?;
    println!(
        "读入 {total} 条，写入旁路表 {} 条；打回：from 越界 {bad_from}、义项变多 {too_many}、漏掉源义项 {uncovered}、解析失败 {missing}",
        kept.len()
    );
    Ok(())
}

// ─────────────────────────── 入口 ───────────────────────────

pub fn main(args: &[String]) -> Result<()> {
    let mut index = PathBuf::from("data/index");
    let mut work = PathBuf::from("data/llm-work.jsonl");
    let mut results = PathBuf::from("data/llm-results.jsonl");
    let (mut jobs, mut batch, mut limit) = (6usize, 25usize, usize::MAX);
    let mut model = String::from("sonnet");

    let cmd = args.first().map(String::as_str).unwrap_or("");
    let mut i = 1;
    while i < args.len() {
        let (k, v) = (args[i].as_str(), args.get(i + 1));
        match (k, v) {
            ("--index", Some(v)) => index = PathBuf::from(v),
            ("--work", Some(v)) => work = PathBuf::from(v),
            ("--results", Some(v)) => results = PathBuf::from(v),
            ("--jobs", Some(v)) => jobs = v.parse().unwrap_or(jobs),
            ("--batch", Some(v)) => batch = v.parse().unwrap_or(batch),
            ("--limit", Some(v)) => limit = v.parse().unwrap_or(limit),
            ("--model", Some(v)) => model = v.clone(),
            _ => {
                i += 1;
                continue;
            }
        }
        i += 2;
    }

    match cmd {
        "export" => export(&index, &work, limit),
        "run" => run(&work, &results, jobs, batch, &model),
        "import" => import(&index, &results),
        _ => bail!("用法: dict-build llm <export|run|import> [选项]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_objects_ignoring_braces_inside_strings() {
        let s = r#"[{"id":1,"t":"a{b}c"},{"id":2}]"#;
        let v = split_objects(s);
        assert_eq!(v.len(), 2);
        assert!(v[0].contains("a{b}c"));
    }

    #[test]
    fn strips_markdown_fences() {
        assert_eq!(strip_fence("```json\n[{\"id\":1}]\n```"), "[{\"id\":1}]");
        assert_eq!(strip_fence("[{\"id\":1}]"), "[{\"id\":1}]");
    }

    #[test]
    fn rejects_ids_not_in_the_batch() {
        let ids: HashSet<u32> = [1u32, 2].into_iter().collect();
        let reply = r#"[{"id":1,"senses":[{"from":[0],"text":"ok","reg":"","col":""}]},
                        {"id":99,"senses":[{"from":[0],"text":"bad","reg":"","col":""}]}]"#;
        let outs = parse_outs(reply, &ids);
        assert_eq!(outs.len(), 1);
        assert_eq!(outs[0].id, 1);
    }

    #[test]
    fn drops_senses_without_text() {
        let ids: HashSet<u32> = [7u32].into_iter().collect();
        let reply = r#"[{"id":7,"senses":[{"from":[0],"text":"","reg":"","col":""}]}]"#;
        assert!(parse_outs(reply, &ids).is_empty(), "空 text 的义项应被丢掉");
    }

    #[test]
    fn parses_a_realistic_reply() {
        let ids: HashSet<u32> = [42u32].into_iter().collect();
        let reply = r#"```json
[{"id":42,"senses":[
  {"from":[0,3],"text":"to compile (source code)","reg":"computing","col":"编译器 / 编译错误"},
  {"from":[1],"text":"to compile; to edit","reg":"","col":""}]}]
```"#;
        let outs = parse_outs(reply, &ids);
        assert_eq!(outs.len(), 1);
        assert_eq!(outs[0].senses.len(), 2);
        assert_eq!(outs[0].senses[0].from, vec![0, 3]);
        assert_eq!(outs[0].senses[0].reg, "computing");
        assert!(outs[0].senses[0].col.contains('/'));
    }

    #[test]
    fn round_trips_through_our_own_json() {
        let o = Out {
            id: 5,
            senses: vec![OutSense {
                from: vec![1, 2],
                text: "a \"quoted\" thing".into(),
                reg: "coll.".into(),
                col: String::new(),
            }],
        };
        let ids: HashSet<u32> = [5u32].into_iter().collect();
        let back = parse_outs(&format!("[{}]", out_json(&o)), &ids);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].senses[0].text, "a \"quoted\" thing");
        assert_eq!(back[0].senses[0].from, vec![1, 2]);
    }

    #[test]
    fn task_json_escapes_and_is_parseable() {
        let t = Task {
            id: 9,
            word: "编译".into(),
            pos: "动".into(),
            src: vec!["to compile \"x\"".into(), "line\nbreak".into()],
            ex: vec![],
        };
        let j = t.to_json();
        assert_eq!(json_u32(&j, "id"), Some(9));
        assert!(!j.contains('\n'), "换行必须转义掉，否则 JSONL 一行会断成两行");
    }
}
