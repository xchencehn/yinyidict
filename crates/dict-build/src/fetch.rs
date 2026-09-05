//! `dict-build fetch`：把全部数据源和运行库拉下来解开。
//!
//! 和 `tools/fetch-data.sh` 是同一件事，区别在于**这个不需要 bash**。
//! 发布包里的用户只有一个 Windows，没有 Git Bash、没有 gzip、没有 bunzip2；
//! 而这些数据又不能跟着发布包一起分发（词库授权见 README），
//! 所以下载解压这一步必须由程序自己完成。
//!
//! 外部只依赖 Windows 自带的两个命令：
//! - `curl.exe`  —— 免掉一整套 HTTP + TLS 依赖
//! - `tar.exe`   —— bsdtar，读 zip 和 tar.bz2 都行
//!
//! 剩下两种它办不了的，用纯 Rust 解：
//! - `.gz` 单文件 → `flate2`（miniz_oxide 后端，不编 C）
//! - `.bz2` 单文件 → `bzip2-rs`。**bsdtar 读不了裸 bz2**（只认归档，
//!   哪怕它自己链着 bz2lib），实测报 `Unrecognized archive format`。
//!   Tatoeba 的两个句子文件恰好就是裸 bz2。

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};

/// 一个要拉的东西。
struct Item {
    /// 下载到哪（相对各自的根目录）。
    file: &'static str,
    url: &'static str,
    /// 拿到之后怎么解开。
    open: How,
    /// 解开后应该出现的东西；已经在了就整个跳过。
    done: &'static str,
}

enum How {
    /// 本来就是明文，下完就完事。
    Plain,
    /// 单文件 gzip。
    Gz(&'static str),
    /// 单文件 bzip2。
    Bz2(&'static str),
    /// 交给 tar.exe。`Some(子目录)` 表示解到子目录里去。
    Tar(Option<&'static str>),
}

const RAW: &[Item] = &[
    Item {
        file: "cedict.txt.gz",
        url: "https://www.mdbg.net/chinese/export/cedict/cedict_1_0_ts_utf-8_mdbg.txt.gz",
        open: How::Gz("cedict.txt"),
        done: "cedict.txt",
    },
    // 中文词频 + 词性。原设计用 SUBTLEX-CH，它的下载已失效，见 CLAUDE.md
    Item {
        file: "jieba_dict.txt",
        url: "https://raw.githubusercontent.com/fxsjy/jieba/master/jieba/dict.txt",
        open: How::Plain,
        done: "jieba_dict.txt",
    },
    Item {
        file: "Unihan.zip",
        url: "https://www.unicode.org/Public/UCD/latest/ucd/Unihan.zip",
        open: How::Tar(Some("unihan")),
        done: "unihan/Unihan_Variants.txt",
    },
    Item {
        file: "ecdict.zip",
        url: "https://github.com/skywind3000/ECDICT/releases/download/1.0.28/ecdict-sqlite-28.zip",
        open: How::Tar(Some("ecdict")),
        done: "ecdict/stardict.db",
    },
    Item {
        file: "cmn_sent.tsv.bz2",
        url: "https://downloads.tatoeba.org/exports/per_language/cmn/cmn_sentences.tsv.bz2",
        open: How::Bz2("cmn_sent.tsv"),
        done: "cmn_sent.tsv",
    },
    Item {
        file: "eng_sent.tsv.bz2",
        url: "https://downloads.tatoeba.org/exports/per_language/eng/eng_sentences.tsv.bz2",
        open: How::Bz2("eng_sent.tsv"),
        done: "eng_sent.tsv",
    },
    Item {
        file: "links.tar.bz2",
        url: "https://downloads.tatoeba.org/exports/links.tar.bz2",
        open: How::Tar(None),
        done: "links.csv",
    },
];

const VENDOR: &[Item] = &[Item {
    file: "sherpa.tar.bz2",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.7/\
          sherpa-onnx-v1.13.7-win-x64-shared-MT-Release.tar.bz2",
    open: How::Tar(None),
    done: "sherpa-onnx-v1.13.7-win-x64-shared-MT-Release/lib/sherpa-onnx-c-api.dll",
}];

const MODELS: &[Item] = &[Item {
    file: "kokoro.tar.bz2",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/\
          kokoro-multi-lang-v1_1.tar.bz2",
    open: How::Tar(None),
    done: "kokoro-multi-lang-v1_1/tokens.txt",
}];

pub fn main(argv: &[String]) -> Result<()> {
    let root = argv
        .iter()
        .position(|a| a == "--root")
        .and_then(|i| argv.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    for (dir, items, what) in [
        (root.join("data/raw"), RAW, "词典数据"),
        (root.join("vendor"), VENDOR, "语音运行库"),
        (root.join("models"), MODELS, "语音模型"),
    ] {
        println!("\n── {what} → {}", dir.display());
        std::fs::create_dir_all(&dir).with_context(|| format!("建不了目录 {}", dir.display()))?;
        for it in items {
            one(&dir, it)?;
        }
    }
    println!("\n数据齐了。接着跑 dict-build 生成词库。");
    Ok(())
}

fn one(dir: &Path, it: &Item) -> Result<()> {
    if dir.join(it.done).exists() {
        println!("  已有 {}", it.done);
        return Ok(());
    }
    let dl = dir.join(it.file);
    if !dl.exists() || std::fs::metadata(&dl).map(|m| m.len()).unwrap_or(0) == 0 {
        println!("  下载 {} …", it.file);
        curl(&dl, it.url)?;
    }

    match it.open {
        How::Plain => {}
        How::Gz(out) => {
            println!("  解开 {} …", it.file);
            let f = std::fs::File::open(&dl)?;
            let mut d = flate2::read::GzDecoder::new(std::io::BufReader::new(f));
            let mut buf = Vec::new();
            d.read_to_end(&mut buf).with_context(|| format!("解 gzip 失败: {}", it.file))?;
            std::fs::write(dir.join(out), buf)?;
        }
        How::Bz2(out) => {
            println!("  解开 {} …", it.file);
            let f = std::fs::File::open(&dl)?;
            let mut d = bzip2_rs::DecoderReader::new(std::io::BufReader::new(f));
            let mut buf = Vec::new();
            d.read_to_end(&mut buf).with_context(|| format!("解 bzip2 失败: {}", it.file))?;
            std::fs::write(dir.join(out), buf)?;
        }
        How::Tar(sub) => {
            println!("  解开 {} …", it.file);
            let into = match sub {
                Some(s) => {
                    let d = dir.join(s);
                    std::fs::create_dir_all(&d)?;
                    d
                }
                None => dir.to_path_buf(),
            };
            tar(&dl, &into)?;
        }
    }

    if !dir.join(it.done).exists() {
        bail!("解开 {} 之后还是找不到 {}", it.file, it.done);
    }
    // 压缩包留着没用，而且都不小
    if !matches!(it.open, How::Plain) {
        let _ = std::fs::remove_file(&dl);
    }
    Ok(())
}

/// 用 Windows 自带的 curl 下载。
///
/// 走子进程而不是引一个 HTTP 客户端：那会拖进整套 TLS 依赖，而这个工程
/// 一向是能不添依赖就不添（见 CLAUDE.md「零 C 依赖」那一节的同一条思路）。
fn curl(to: &Path, url: &str) -> Result<()> {
    // URL 常量里为了排版折了行，这里把续行留下的空白去掉
    let url: String = url.split_whitespace().collect();
    let st = std::process::Command::new("curl.exe")
        .args(["-sSL", "--retry", "3", "--fail", "-o"])
        .arg(to)
        .arg(&url)
        .status()
        .context("起不来 curl.exe（Windows 10 以上自带）")?;
    if !st.success() {
        bail!("下载失败（{st}）：{url}");
    }
    Ok(())
}

/// 用 Windows 自带的 tar（bsdtar）解归档。zip 和 tar.bz2 它都认。
fn tar(archive: &Path, into: &Path) -> Result<()> {
    let st = std::process::Command::new("tar.exe")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .status()
        .context("起不来 tar.exe（Windows 10 以上自带）")?;
    if !st.success() {
        bail!("解包失败（{st}）：{}", archive.display());
    }
    Ok(())
}
