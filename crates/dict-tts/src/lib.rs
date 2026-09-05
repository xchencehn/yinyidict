//! 本地 TTS：sherpa-onnx（dlopen 预编译 DLL）+ winmm 播放。
//!
//! **词头要包进一句话里念**，见 [`carrier`] —— 神经 TTS 喂单个词时韵律会
//! 退化成平调，这跟模型好坏无关，Kokoro 也一样。整句都念出来，不做裁剪。
//!
//! 合成在独立线程上跑，界面不会因为它卡帧。合成结果带缓存，打开词条页时会
//! 预热 —— 没有独显的机器上 CPU 合成一次以百毫秒到秒计，不预热就得干等。

pub mod audio;
pub mod discover;
mod ffi;

use anyhow::{bail, Context, Result};
use dict_core::dynlib::DynLib;
use std::ffi::{c_void, CString};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct Config {
    /// 放 `sherpa-onnx-c-api.dll` 和 `onnxruntime.dll` 的目录。
    pub lib_dir: PathBuf,
    /// 模型目录。
    pub model_dir: PathBuf,
    /// `"cpu"` 或 `"cuda"`。CUDA 需要另配 onnxruntime 的 CUDA provider。
    pub provider: String,
    pub num_threads: i32,
    /// 额外加进 DLL 搜索路径的目录。CUDA 后端要用 ——
    /// `onnxruntime_providers_cuda.dll` 会去找 cudnn64_9 / cublas64_13 等等，
    /// 它们分散在 cuDNN 和 CUDA Toolkit 各自的 bin 下。
    pub dll_dirs: Vec<PathBuf>,
}

/// 中文和英文都用这一个音色，不给选。
///
/// Kokoro v1.1-zh 的 103 个音色里，0 是英文女声 `af_maple`。它念中文是带口音，
/// 但**吐字比 3-102 那批中文音色都清楚** —— 挑音色时把各家中文模型连同
/// Kokoro 自带的中文音色都听了一轮，普遍含混。查词要的是听清是哪个字，
/// 不是听着像母语者。
///
/// 所以设置里也不再给音色选项：挑不出更好的，摆在那里只是噪音。
const VOICE: i32 = 0;

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Loading,
    Ready { sample_rate: u32, model: String },
    Failed(String),
}

/// 一次合成的耗时。RTF = 合成墙钟时间 / 音频时长。
///
/// 构建期是批处理，跑十小时无所谓；真正有实时要求的只有「点了播放键才合成」
/// 这一下。RTF < 0.2 时例句可以现场合成，> 0.5 就得预生成。
#[derive(Clone, Copy, Debug)]
pub struct Bench {
    pub synth_ms: f64,
    pub audio_ms: f64,
    pub rtf: f64,
}

enum Msg {
    /// 念出来。
    Play {
        text: String,
    },
    /// 只合成进缓存、不播放。打开词条页时先跑一遍，点播放时就是零等待。
    Warm {
        text: String,
    },
    /// 只合成不播放，把耗时送回去。
    Bench {
        text: String,
        reply: Sender<Option<Bench>>,
    },
    Stop,
}

/// 把一个词头包成一句话。
///
/// 神经 TTS 在无上下文的短文本上韵律会退化成平调 —— 念「将就」两个字，
/// 出来的是两个等长等高的音节。包进一句话里，韵律模型才有东西可依据。
///
/// **整句都念出来，不裁剪。** 早先的版本会按最后一处停顿把词切下来只播那一段，
/// 听下来不如整句自然，而且裁剪本身还会引入边界瑕疵。
///
/// 逗号是有用的：它逼出一个短停顿，让后面那个词更容易听清。
pub fn carrier(word: &str) -> String {
    if word.chars().any(dict_core::is_cjk) {
        format!("这个词是，{word}")
    } else {
        format!("The word is, {word}")
    }
}

/// 对外句柄。真正的引擎活在工作线程上。
pub struct Tts {
    tx: Sender<Msg>,
    status: Arc<Mutex<Status>>,
}

impl Tts {
    /// 起一个工作线程去加载模型。本函数立即返回，加载进度看 [`Tts::status`]。
    pub fn start(cfg: Config) -> Tts {
        let (tx, rx) = std::sync::mpsc::channel();
        let status = Arc::new(Mutex::new(Status::Loading));
        let st = Arc::clone(&status);
        std::thread::Builder::new()
            .name("tts".into())
            .spawn(move || worker(cfg, rx, st))
            .expect("无法创建 TTS 线程");
        Tts { tx, status }
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or(Status::Loading)
    }

    /// 念一个词头：包成一句话再念，见 [`carrier`]。
    pub fn say_word(&self, word: &str) -> Result<()> {
        self.say(&carrier(word))
    }

    /// 预热一个词头。
    pub fn prefetch_word(&self, word: &str) {
        self.prefetch(&carrier(word));
    }

    /// 念一段现成的文字（例句用，它本来就是句子，不必再包）。
    pub fn say(&self, text: &str) -> Result<()> {
        self.tx.send(Msg::Play { text: text.to_string() }).context("TTS 线程已退出")
    }

    /// 预热，只进缓存不出声。
    ///
    /// 没有独显的机器上 CPU 合成一次以秒计，打开词条页就先算好，
    /// 点播放时才不用等。
    pub fn prefetch(&self, text: &str) {
        let _ = self.tx.send(Msg::Warm { text: text.to_string() });
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Msg::Stop);
    }

    /// 合成一次但不播放，量出本机的 RTF。阻塞直到工作线程算完。
    pub fn bench(&self, text: &str) -> Result<Bench> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx.send(Msg::Bench { text: text.to_string(), reply: tx }).context("TTS 线程已退出")?;
        rx.recv().context("TTS 线程没有回应")?.context("合成失败")
    }
}

fn worker(cfg: Config, rx: Receiver<Msg>, status: Arc<Mutex<Status>>) {
    let set = |s: Status| {
        if let Ok(mut g) = status.lock() {
            *g = s;
        }
    };

    // CUDA 起不来就退回 CPU：发音不该因为显卡环境不全而整个没了
    let loaded = Engine::load(&cfg).or_else(|e| {
        if cfg.provider == "cpu" {
            return Err(e);
        }
        eprintln!("TTS: {} 后端加载失败（{e:#}），回落到 CPU", cfg.provider);
        Engine::load(&Config { provider: "cpu".into(), ..cfg.clone() })
    });

    let mut engine = match loaded {
        Ok(e) => {
            set(Status::Ready { sample_rate: e.sample_rate, model: e.model_name.clone() });
            e
        }
        Err(err) => {
            set(Status::Failed(format!("{err:#}")));
            return;
        }
    };

    while let Ok(first) = rx.recv() {
        // 把已经排队的消息一次取干净再决定做什么。三类消息的处理方式不同：
        //   Bench    —— 调用方在阻塞等回信，一条都不能丢；
        //   播放请求 —— 连点时只做最后一条；
        //   预取     —— 优先级最低，有播放请求时直接让路。
        let mut queue = vec![first];
        while let Ok(next) = rx.try_recv() {
            queue.push(next);
        }

        let mut benches = Vec::new();
        let mut play: Option<Msg> = None;
        // 预热攒着一起做：打开词条页会一次发好几条（词头 + 例句），
        // 一条都不能少；但快速翻词条时积压的旧请求会连着排在后面，
        // 所以只留最后一批的量 —— 这里靠「有新的播放请求就先让路」来兜。
        let mut warm: Vec<Msg> = Vec::new();
        for m in queue {
            match m {
                Msg::Bench { .. } => benches.push(m),
                Msg::Play { .. } => play = Some(m),
                Msg::Warm { .. } => warm.push(m),
                Msg::Stop => {
                    engine.player.stop();
                    play = None;
                }
            }
        }

        let run = |m: Msg, engine: &mut Engine| {
            let r = match m {
                Msg::Play { text } => engine.speak(&text),
                Msg::Warm { text } => engine.warm(&text),
                Msg::Bench { text, reply } => {
                    let t = std::time::Instant::now();
                    let r = engine.synth(&text, VOICE);
                    let out = r.as_ref().ok().map(|s| {
                        let synth_ms = t.elapsed().as_secs_f64() * 1000.0;
                        let audio_ms = s.len() as f64 / engine.sample_rate as f64 * 1000.0;
                        Bench { synth_ms, audio_ms, rtf: synth_ms / audio_ms.max(f64::EPSILON) }
                    });
                    let _ = reply.send(out);
                    r.map(|_| ())
                }
                Msg::Stop => Ok(()),
            };
            if let Err(e) = r {
                eprintln!("TTS: {e:#}");
            }
        };

        for m in benches {
            run(m, &mut engine);
        }
        match play {
            Some(m) => run(m, &mut engine),
            None => {
                for m in warm {
                    run(m, &mut engine);
                }
            }
        }
    }
}

/// 模型族，靠目录里的文件认：有 `voices.bin` 的是 Kokoro（音色嵌入单独一个
/// 文件），其余按 VITS（单文件端到端）处理。
enum Family {
    Kokoro,
    Vits,
}

/// 合成结果缓存的上限。词典里一个词常被反复点，命中率很高；
/// 24 kHz 单声道 f32，一秒约 96 KB，例句最长几秒，128 条封顶几十 MB。
const CACHE_CAP: usize = 128;

struct Engine {
    // 这三个字段的析构顺序有讲究：句柄先于库释放。
    handle: *const c_void,
    generate: ffi::FnGenerate,
    free_audio: ffi::FnFreeAudio,
    destroy: ffi::FnDestroy,
    sample_rate: u32,
    model_name: String,
    player: audio::Player,
    /// 文本 → 合成好的样本。插入顺序另存在 `cache_order` 里做淘汰。
    cache: std::collections::HashMap<String, Vec<f32>>,
    cache_order: std::collections::VecDeque<String>,
    /// 预加载的 CUDA/cuDNN 模块，必须活到引擎销毁。
    _preloaded: Vec<DynLib>,
    _sherpa: DynLib,
    _ort: DynLib,
}

fn first_existing(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().map(|n| dir.join(n)).find(|p| p.exists())
}

fn any_onnx(dir: &Path) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "onnx"))
        .collect();
    v.sort();
    v.into_iter().next()
}

/// 文本正则化的 rule fst（数字、日期、电话、多音字），中文模型基本都带。
///
/// 命名各家不一（kokoro 叫 `number-zh.fst`，别家常叫 `number.fst`），所以
/// 直接把目录下所有 `.fst` 都收进来，不去猜文件名。
fn rule_fsts(dir: &Path) -> String {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "fst"))
        .filter_map(|p| p.to_str().map(str::to_string))
        .collect();
    v.sort();
    v.join(",")
}

impl Engine {
    fn load(cfg: &Config) -> Result<Engine> {
        let md = &cfg.model_dir;
        if !md.is_dir() {
            bail!("模型目录不存在: {}", md.display());
        }
        // CUDA 依赖要先按绝对路径装进进程。
        //
        // 光改 PATH 不够：onnxruntime 是用带 LOAD_LIBRARY_SEARCH_* 标志的方式
        // 加载 provider 的，那条路径不搜 PATH，于是报
        // 「onnxruntime_providers_cuda.dll ... depends on cudnn64_9.dll which is missing」。
        // 但加载器有一条规则救得了我们：同名模块只要已经在进程里，就直接复用。
        // 所以按依赖顺序自己 dlopen 一遍。PATH 也照改 —— cuDNN 运行时还会自己
        // 按名字去 dlopen 它的子引擎库，那条路是走 PATH 的。
        let preloaded = if cfg.dll_dirs.is_empty() {
            Vec::new()
        } else {
            let mut path = std::env::var("PATH").unwrap_or_default();
            for d in &cfg.dll_dirs {
                path = format!("{};{path}", d.display());
            }
            std::env::set_var("PATH", path);
            preload_cuda(&cfg.dll_dirs)
        };

        // onnxruntime 必须先按全路径装进来，之后 sherpa 的导入才解析得到它
        let ort = DynLib::open_path(&cfg.lib_dir.join("onnxruntime.dll"))
            .context("加载 onnxruntime.dll 失败")?;
        let sherpa = DynLib::open_path(&cfg.lib_dir.join("sherpa-onnx-c-api.dll"))
            .context("加载 sherpa-onnx-c-api.dll 失败")?;

        // SAFETY: 符号名和签名均已对照 c-api.h 与 DLL 导出表核实。
        let (create, destroy, sample_rate_fn, generate, free_audio) = unsafe {
            (
                sherpa.sym::<ffi::FnCreate>("SherpaOnnxCreateOfflineTts")?,
                sherpa.sym::<ffi::FnDestroy>("SherpaOnnxDestroyOfflineTts")?,
                sherpa.sym::<ffi::FnSampleRate>("SherpaOnnxOfflineTtsSampleRate")?,
                sherpa.sym::<ffi::FnGenerate>("SherpaOnnxOfflineTtsGenerateWithConfig")?,
                sherpa.sym::<ffi::FnFreeAudio>("SherpaOnnxDestroyOfflineTtsGeneratedAudio")?,
            )
        };

        let family = if md.join("voices.bin").exists() { Family::Kokoro } else { Family::Vits };
        let model = first_existing(md, &["model.onnx", "model.int8.onnx"])
            .or_else(|| any_onnx(md))
            .with_context(|| format!("{} 里没有 .onnx 模型", md.display()))?;
        let tokens = md.join("tokens.txt");
        if !tokens.exists() {
            bail!("{} 里缺 tokens.txt", md.display());
        }

        // 这些 CString 必须活到 create() 返回：配置结构里存的是裸指针。
        let mut keep: Vec<CString> = Vec::new();
        let mut cs = |s: String| -> *const std::ffi::c_char {
            let c = CString::new(s).unwrap_or_default();
            let p = c.as_ptr();
            keep.push(c);
            p
        };
        let path = |p: &Path| p.to_string_lossy().into_owned();
        let opt = |p: PathBuf| if p.exists() { path(&p) } else { String::new() };

        let mut conf =
            ffi::TtsConfig { max_num_sentences: 2, silence_scale: 1.0, ..Default::default() };
        conf.model.num_threads = cfg.num_threads;
        conf.model.debug = 0;
        conf.model.provider = cs(cfg.provider.clone());
        conf.rule_fsts = cs(rule_fsts(md));
        conf.rule_fars = cs(String::new());

        match family {
            Family::Kokoro => {
                conf.model.kokoro.model = cs(path(&model));
                conf.model.kokoro.voices = cs(path(&md.join("voices.bin")));
                conf.model.kokoro.tokens = cs(path(&tokens));
                conf.model.kokoro.data_dir = cs(opt(md.join("espeak-ng-data")));
                conf.model.kokoro.dict_dir = cs(opt(md.join("dict")));
                conf.model.kokoro.lexicon = cs(lexicons(md));
                conf.model.kokoro.lang = cs(String::new());
                conf.model.kokoro.length_scale = 1.0;
            }
            Family::Vits => {
                conf.model.vits.model = cs(path(&model));
                conf.model.vits.tokens = cs(path(&tokens));
                conf.model.vits.lexicon = cs(lexicons(md));
                conf.model.vits.data_dir = cs(opt(md.join("espeak-ng-data")));
                conf.model.vits.dict_dir = cs(opt(md.join("dict")));
                conf.model.vits.noise_scale = 0.667;
                conf.model.vits.noise_scale_w = 0.8;
                conf.model.vits.length_scale = 1.0;
            }
        }

        // SAFETY: conf 的每个指针都指向 keep 里仍然存活的 CString；
        // create 会把配置内容复制走，返回后 keep 即可释放。
        let handle = unsafe { create(&conf) };
        drop(keep);
        if handle.is_null() {
            bail!("sherpa-onnx 创建 TTS 失败（模型目录 {}）", md.display());
        }

        // SAFETY: handle 非空，由 create 返回。
        let sr = unsafe { sample_rate_fn(handle) };

        Ok(Engine {
            handle,
            generate,
            free_audio,
            destroy,
            sample_rate: sr.max(1) as u32,
            model_name: md
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            player: audio::Player::new()?,
            cache: std::collections::HashMap::new(),
            cache_order: std::collections::VecDeque::new(),
            _preloaded: preloaded,
            _sherpa: sherpa,
            _ort: ort,
        })
    }

    /// 合成一段文本，拿回 f32 样本。
    fn synth(&self, text: &str, sid: i32) -> Result<Vec<f32>> {
        let c = CString::new(text).context("待合成文本含有 NUL 字节")?;
        let gc = ffi::GenerationConfig { sid, ..Default::default() };
        // SAFETY: handle 有效；c 在调用期间存活；返回的音频由 free_audio 释放。
        unsafe {
            let a = (self.generate)(
                self.handle,
                c.as_ptr(),
                &gc,
                std::ptr::null(),
                std::ptr::null_mut(),
            );
            if a.is_null() {
                bail!("合成返回空");
            }
            let n = (*a).n.max(0) as usize;
            let out = if n == 0 || (*a).samples.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts((*a).samples, n).to_vec()
            };
            (self.free_audio)(a);
            Ok(out)
        }
    }

    /// 取一段文字的波形，带缓存。
    fn samples(&mut self, text: &str) -> Result<Vec<f32>> {
        if let Some(v) = self.cache.get(text) {
            return Ok(v.clone());
        }
        let out = self.synth(text, VOICE)?;
        self.cache.insert(text.to_string(), out.clone());
        self.cache_order.push_back(text.to_string());
        while self.cache_order.len() > CACHE_CAP {
            if let Some(old) = self.cache_order.pop_front() {
                self.cache.remove(&old);
            }
        }
        Ok(out)
    }

    fn speak(&mut self, text: &str) -> Result<()> {
        let s = self.samples(text)?;
        self.play(&s)
    }

    /// 只算进缓存，不出声。
    fn warm(&mut self, text: &str) -> Result<()> {
        self.samples(text).map(|_| ())
    }

    fn play(&mut self, samples: &[f32]) -> Result<()> {
        if samples.is_empty() {
            bail!("合成结果为空");
        }
        self.player.play_wav(audio::to_wav(samples, self.sample_rate))
    }
}

/// 按依赖顺序把 CUDA/cuDNN 的 DLL 装进进程。装不上的直接跳过 ——
/// 不同 CUDA 版本带的库并不一样，缺哪个由后面真正的加载去报错。
fn preload_cuda(dirs: &[PathBuf]) -> Vec<DynLib> {
    // 越靠前越先装。cudnn64_9 放最后：它是门面，要先有底下那些。
    const ORDER: &[&str] = &[
        "cudart64_",
        "nvrtc",
        "cublasLt64_",
        "cublas64_",
        "cufft64_",
        "curand64_",
        "cusparse64_",
        "cudnn_",
        "cudnn64_",
    ];
    let rank = |name: &str| ORDER.iter().position(|p| name.starts_with(p));

    let mut files: Vec<(usize, PathBuf)> = dirs
        .iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().filter_map(|e| e.ok()))
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll")))
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_string();
            rank(&name).map(|r| (r, p))
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    files.into_iter().filter_map(|(_, p)| DynLib::open_path(&p).ok()).collect()
}

/// 模型自带的词典文件，可能有多个，用逗号连起来。
fn lexicons(dir: &Path) -> String {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("lexicon") && s.ends_with(".txt"))
        })
        .filter_map(|p| p.to_str().map(str::to_string))
        .collect();
    v.sort();
    v.join(",")
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.player.stop();
        // SAFETY: handle 只在这里销毁一次。
        unsafe {
            (self.destroy)(self.handle);
        }
    }
}

// Engine 只在它自己的工作线程上被创建和使用；跨线程的只有 Tts 句柄里的 Sender。
unsafe impl Send for Engine {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 载体句的语言跟着词走，而不是跟着界面走 ——
    /// 中文词条页里也可能点到英文词，反过来也一样。
    #[test]
    fn the_carrier_follows_the_word_not_the_ui() {
        assert_eq!(carrier("将就"), "这个词是，将就");
        assert_eq!(carrier("compile"), "The word is, compile");
        // 混着的按中文算：只要有汉字，中文那句读起来就不会错
        assert_eq!(carrier("C 语言"), "这个词是，C 语言");
    }

    /// 逗号不能掉 —— 它逼出的那个短停顿是词能被听清的原因。
    #[test]
    fn the_carrier_keeps_the_pause() {
        assert!(carrier("银行").contains('，'));
        assert!(carrier("bank").contains(','));
    }
}
