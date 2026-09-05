//! 本地中英词典。
//!
//! 一个输入框，下面一块区域。中文词和英文词都是一等词头，各有独立词条页。
//! 完全离线：词库是构建期生成的 mmap 索引，发音是本机 sherpa-onnx 现场合成。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod autostart;
mod fonts;
mod icon;
mod settings;
mod shot;
mod theme;
mod tray;

use anyhow::{Context, Result};
use dict_core::Store;
use std::path::{Path, PathBuf};

/// 依次在这些位置找资源：当前目录、可执行文件旁边、以及它们的上级。
///
/// 这样既能 `cargo run` 从工程根跑，也能把 exe 和 data/ 一起拷到别处跑。
fn roots() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        v.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut p = exe.parent().map(Path::to_path_buf);
        // target/release/dict.exe → 往上找到工程根
        for _ in 0..4 {
            let Some(d) = p else { break };
            v.push(d.clone());
            p = d.parent().map(Path::to_path_buf);
        }
    }
    v
}

fn find(rel: &str) -> Option<PathBuf> {
    roots().into_iter().map(|r| r.join(rel)).find(|p| p.exists())
}

/// 挑运行库和执行后端。
///
/// CUDA 版运行库、cuDNN、CUDA Toolkit 三样齐全才走 GPU，缺一样就用 CPU。
/// 即便选了 CUDA，`dict_tts` 在引擎起不来时还会再自己回落一次。
/// `DICT_TTS_PROVIDER=cpu|cuda` 可以强制。
fn pick_backend() -> Option<(PathBuf, String, Vec<PathBuf>)> {
    use dict_tts::discover;

    let mut libs: Vec<PathBuf> = Vec::new();
    let mut cuda_dirs: Vec<PathBuf> = Vec::new();
    for r in roots() {
        libs.extend(discover::sherpa_libs(&r.join("vendor")));
        if cuda_dirs.is_empty() {
            cuda_dirs = discover::cuda_dll_dirs(&r.join("vendor/cuda"));
        }
    }
    libs.sort();
    libs.dedup();

    let want_cuda = match std::env::var("DICT_TTS_PROVIDER").as_deref() {
        Ok("cuda") => true,
        Ok("cpu") => false,
        _ => !cuda_dirs.is_empty() && libs.iter().any(|p| discover::is_cuda(p)),
    };
    if want_cuda {
        if let Some(lib) = libs.iter().find(|p| discover::is_cuda(p)) {
            return Some((lib.clone(), String::from("cuda"), cuda_dirs));
        }
    }
    let cpu = libs.iter().find(|p| !discover::is_cuda(p)).or_else(|| libs.first())?;
    Some((cpu.clone(), String::from("cpu"), Vec::new()))
}

fn main() {
    // 没有控制台，出错必须弹窗，否则双击之后就是「什么都没发生」
    if let Err(e) = run() {
        tray::alert("词典启动失败", &format!("{e:#}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let index = find("data/index").context(
        "还没有词库。

         先双击同目录下的 setup.bat，它会下载词典数据并建好索引
         （约 850 MB，头一次要等几分钟）。",
    )?;
    let store = Store::open(&index)?;
    println!("词库 {} 条 · {}", store.n, index.display());

    // 发音是可选的：没有模型或 DLL 也应该能正常查词
    let (tts, note) = match (pick_backend(), pick_model()) {
        (Some((lib_dir, provider, dll_dirs)), Some(model_dir)) => {
            println!("发音 {provider} · {}", model_dir.display());
            let cfg = dict_tts::Config {
                lib_dir,
                model_dir,
                provider,
                dll_dirs,
                num_threads: std::thread::available_parallelism()
                    .map(|n| (n.get() / 2).clamp(2, 8) as i32)
                    .unwrap_or(4),
            };
            (Some(cfg), String::new())
        }
        (None, _) => (None, "没找到 sherpa-onnx 运行库，发音不可用".to_string()),
        (_, None) => (None, "没找到 models/ 下的语音模型，发音不可用".to_string()),
    };
    if !note.is_empty() {
        eprintln!("{note}");
    }
    // 这两件事都在启动路径外面做，见 app::Deferred
    let deferred = app::Deferred { tts, index };

    let cfg_path = settings::path(&roots());
    let cfg = settings::Settings::load(&cfg_path);
    println!("设置 {}", cfg_path.display());

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // 尺寸取上次退出时记下的那个
            .with_inner_size(cfg.window)
            .with_min_inner_size(app::WINDOW_MIN)
            .with_icon(icon::window_icon())
            // 标题栏自己画：系统那条在这套配色里格格不入，而且要往里塞一颗
            // 「钉在最前」的钉子 —— 原生标题栏加不了按钮。
            .with_decorations(false)
            .with_title("词典"),
        ..Default::default()
    };
    eframe::run_native(
        "词典",
        opts,
        Box::new(move |cc| {
            Ok(Box::new(app::App::new(cc, store, deferred, note, cfg, cfg_path))
                as Box<dyn eframe::App>)
        }),
    )
    .map_err(|e| anyhow::anyhow!("窗口启动失败: {e}"))
}

/// 后台把索引文件读一遍，把它们拉进系统页缓存。
///
/// 词库是 mmap 的，第一次碰到某一页要缺页从磁盘读 —— 冷启动后头几次击键
/// 会看到 9~29 ms，之后才降到 1 ms 以内。这里只预热通道索引和 postings
/// （合计约 60 MB），不碰 entries.bin（351 MB，只有要显示的那条才读）。
///
/// **不要在 `main` 开头就起它。** 60 MB 的顺序读会和创建 GL 上下文、
/// 加载语音模型抢同一块磁盘，实测能把窗口出现的时间拖长好几倍。
/// 由 [`app::App`] 在第一帧画完之后才放出来。
pub fn warm_index(dir: &Path) {
    let dir = dir.to_path_buf();
    std::thread::Builder::new()
        .name("warm".into())
        .spawn(move || {
            let Ok(rd) = std::fs::read_dir(&dir) else { return };
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                let keep = p
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x == "fst" || x == "post");
                if keep {
                    // 读出来就丢，目的只是让内核把页装进来
                    let _ = std::fs::read(&p);
                }
            }
        })
        .ok();
}

/// 挑一个语音模型。优先 Kokoro 多语（中英同一个模型），否则取 models/ 下第一个。
fn pick_model() -> Option<PathBuf> {
    let models = find("models")?;
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&models)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && p.join("tokens.txt").exists())
        .collect();
    dirs.sort();
    dirs.iter()
        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("kokoro")))
        .cloned()
        .or_else(|| dirs.into_iter().next())
}
