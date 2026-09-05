//! 发音冒烟测试 / RTF 实测 / A-B 试听。
//!
//! ```text
//! cargo run --release -p dict-tts --example say -- <模型目录> [--cuda] [--play] [词...]
//! cargo run --release -p dict-tts --example say -- models/kokoro-multi-lang-v1_1
//! ```
//!
//! 默认只测 RTF 不出声（无人值守也能跑）；加 `--play` 才实际念出来。

use dict_tts::{Config, Status, Tts};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 找运行库；`cuda=true` 时只认目录名里带 cuda 的那份。
fn lib_dir(cuda: bool) -> Option<PathBuf> {
    dict_tts::discover::sherpa_libs(std::path::Path::new("vendor"))
        .into_iter()
        .find(|p| dict_tts::discover::is_cuda(p) == cuda)
}

fn wait_ready(tts: &Tts) -> Status {
    let t = Instant::now();
    loop {
        match tts.status() {
            Status::Loading if t.elapsed() < Duration::from_secs(180) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            s => return s,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法: say <模型目录> [--play] [词...]");
        std::process::exit(2);
    }
    let model_dir = PathBuf::from(args.remove(0));
    let play = args.iter().any(|a| a == "--play");
    let cuda = args.iter().any(|a| a == "--cuda");
    args.retain(|a| a != "--play" && a != "--cuda");

    let words: Vec<String> = if args.is_empty() {
        ["将就", "银行", "中国", "编译器", "一诺千金", "compile", "banana", "interrupt"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        args
    };
    // 例句比词头长得多，RTF 要分开量：真正有实时要求的就是点击例句这一下
    let sentences = ["没有热水，你先将就一下。", "The compiler reported a type error."];

    let lib_dir = lib_dir(cuda).ok_or_else(|| {
        anyhow::anyhow!(
            "vendor/ 下找不到{}的 sherpa-onnx 运行库",
            if cuda { " CUDA 版" } else { " CPU 版" }
        )
    })?;
    let (provider, dll_dirs) = if cuda {
        (
            String::from("cuda"),
            dict_tts::discover::cuda_dll_dirs(std::path::Path::new("vendor/cuda")),
        )
    } else {
        (String::from("cpu"), Vec::new())
    };
    let threads =
        std::thread::available_parallelism().map(|n| (n.get() / 2).clamp(2, 8) as i32).unwrap_or(4);

    let tts = Tts::start(Config {
        lib_dir: lib_dir.clone(),
        model_dir: model_dir.clone(),
        provider: provider.clone(),
        dll_dirs: dll_dirs.clone(),
        num_threads: threads,
    });
    let t0 = Instant::now();
    match wait_ready(&tts) {
        Status::Ready { sample_rate, model } => println!(
            "模型 {model} · {sample_rate} Hz · {threads} 线程 · 装载 {:.1}s\n",
            t0.elapsed().as_secs_f32()
        ),
        Status::Failed(e) => anyhow::bail!("加载失败: {e}"),
        Status::Loading => anyhow::bail!("加载超时"),
    }

    println!("{:<34} {:>10} {:>10} {:>8}", "文本", "合成", "音频", "RTF");
    println!("{}", "─".repeat(66));

    let mut worst: f64 = 0.0;
    let mut report = |label: &str, text: &str| -> anyhow::Result<()> {
        // 头一次调用会有一次性开销，量第二次
        let _ = tts.bench(text)?;
        let b = tts.bench(text)?;
        worst = worst.max(b.rtf);
        println!("{label:<34} {:>8.0} ms {:>8.0} ms {:>8.3}", b.synth_ms, b.audio_ms, b.rtf);
        Ok(())
    };

    for w in &words {
        // 词头实际合成的是包好的那句话，按它量才准
        report(&format!("词头 {w}"), &dict_tts::carrier(w))?;
    }
    for s in sentences {
        report(&format!("例句 {}", s.chars().take(12).collect::<String>()), s)?;
    }

    println!("{}", "─".repeat(66));
    println!("最差 RTF = {worst:.3}");
    println!(
        "{}",
        if worst < 0.2 {
            "→ 例句可以点了就现场合成，不必预生成。"
        } else if worst < 0.5 {
            "→ 词头实时没问题；例句现场合成会有可感知等待，可考虑预生成高频词。"
        } else {
            "→ 太慢，例句必须预生成。"
        }
    );

    if play {
        println!(
            "
开始试听"
        );
        for w in &words {
            tts.say_word(w)?;
            println!("   {w}");
            std::thread::sleep(Duration::from_millis(1800));
        }
        for s in sentences {
            tts.say(s)?;
            println!("   {}", s.chars().take(16).collect::<String>());
            std::thread::sleep(Duration::from_millis(3000));
        }
    }

    Ok(())
}
