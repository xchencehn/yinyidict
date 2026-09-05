//! 命令行查词，用来核对打分行为，不进最终界面。
//!
//! ```text
//! cargo run --release -p dict-build --bin dict-query -- 事 china zhonggu
//! cargo run --release -p dict-build --bin dict-query -- --bench
//! cargo run --release -p dict-build --bin dict-query -- --raw 意思
//! ```

use anyhow::Result;
use dict_core::{Params, Store};
use std::time::Instant;

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // 词条页不再显示原始释义了，但重组过的义项总得有办法核对 —— 就是这个开关
    let show_raw = args.iter().any(|a| a == "--raw");
    args.retain(|a| a != "--raw");
    let t0 = Instant::now();
    let store = Store::open("data/index")?;
    println!("词库 {} 条，装载 {:.1} ms\n", store.n, t0.elapsed().as_secs_f64() * 1000.0);

    let p = Params::default();
    let queries: Vec<String> = if args.is_empty() || args[0] == "--bench" {
        [
            "事", "时间", "中", "中国", "zhonggu", "zg", "china", "slow", "man", "compile",
            "compiled", "编译", "banana", "a", "裡面", "着",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    } else {
        args
    };

    for q in &queries {
        let t = Instant::now();
        let r = store.search(q, &p);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "── {q:<10} {ms:>7.3} ms   扫 {} 键{}",
            r.scanned,
            if r.truncated { "  [已截断]" } else { "" }
        );
        for h in &r.hits {
            let e = store.entry(h.id)?;
            if show_raw {
                let mark = if store.is_rewritten(h.id) { "（已重组）" } else { "" };
                println!("   ── {} {}{}", e.word, e.reading, mark);
                for s in &e.senses {
                    let reg =
                        if s.reg.is_empty() { String::new() } else { format!("  ({})", s.reg) };
                    println!("      重组后 {}{reg}", s.text);
                }
                println!("      原  文 {}", e.raw.replace('\n', " / "));
                continue;
            }
            let gloss: Vec<&str> = e.senses.iter().take(2).map(|s| s.text.as_str()).collect();
            println!(
                "   {:>6.2} {:<8} {:<14} {:<18} {}",
                h.score,
                h.lane.label(),
                e.word,
                e.reading,
                gloss.join("; ").chars().take(52).collect::<String>()
            );
        }
        println!();
    }
    Ok(())
}
