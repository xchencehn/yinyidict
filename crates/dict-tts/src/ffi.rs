//! sherpa-onnx 离线 TTS 的 C API 绑定。
//!
//! 这些 `#[repr(C)]` 结构必须和 `sherpa-onnx/c-api/c-api.h` 的字段顺序、类型
//! 逐字对应 —— 结构是**按值**嵌套的，中间任何一个字段错位，后面全部读到垃圾。
//! 即使我们只填 vits / kokoro 两族，其余族的布局也必须完整写出来。
//!
//! 走 dlopen 而不是链接导入库：本机没有 MSVC，预编译的 sherpa DLL 是 MSVC 产物，
//! 但它导出的是纯 C 接口，x86_64 上 MSVC 和 MinGW 的 C 调用约定一致。

use std::ffi::{c_char, c_int, c_void};

#[repr(C)]
#[derive(Default)]
pub struct VitsModelConfig {
    pub model: *const c_char,
    pub lexicon: *const c_char,
    pub tokens: *const c_char,
    pub data_dir: *const c_char,
    pub noise_scale: f32,
    pub noise_scale_w: f32,
    pub length_scale: f32,
    pub dict_dir: *const c_char,
}

#[repr(C)]
#[derive(Default)]
pub struct MatchaModelConfig {
    pub acoustic_model: *const c_char,
    pub vocoder: *const c_char,
    pub lexicon: *const c_char,
    pub tokens: *const c_char,
    pub data_dir: *const c_char,
    pub noise_scale: f32,
    pub length_scale: f32,
    pub dict_dir: *const c_char,
}

#[repr(C)]
#[derive(Default)]
pub struct KokoroModelConfig {
    pub model: *const c_char,
    pub voices: *const c_char,
    pub tokens: *const c_char,
    pub data_dir: *const c_char,
    pub length_scale: f32,
    pub dict_dir: *const c_char,
    pub lexicon: *const c_char,
    pub lang: *const c_char,
}

#[repr(C)]
#[derive(Default)]
pub struct KittenModelConfig {
    pub model: *const c_char,
    pub voices: *const c_char,
    pub tokens: *const c_char,
    pub data_dir: *const c_char,
    pub length_scale: f32,
}

#[repr(C)]
#[derive(Default)]
pub struct ZipvoiceModelConfig {
    pub tokens: *const c_char,
    pub encoder: *const c_char,
    pub decoder: *const c_char,
    pub vocoder: *const c_char,
    pub data_dir: *const c_char,
    pub lexicon: *const c_char,
    pub feat_scale: f32,
    pub t_shift: f32,
    pub target_rms: f32,
    pub guidance_scale: f32,
}

#[repr(C)]
#[derive(Default)]
pub struct PocketModelConfig {
    pub lm_flow: *const c_char,
    pub lm_main: *const c_char,
    pub encoder: *const c_char,
    pub decoder: *const c_char,
    pub text_conditioner: *const c_char,
    pub vocab_json: *const c_char,
    pub token_scores_json: *const c_char,
    pub voice_embedding_cache_capacity: c_int,
}

#[repr(C)]
#[derive(Default)]
pub struct SupertonicModelConfig {
    pub duration_predictor: *const c_char,
    pub text_encoder: *const c_char,
    pub vector_estimator: *const c_char,
    pub vocoder: *const c_char,
    pub tts_json: *const c_char,
    pub unicode_indexer: *const c_char,
    pub voice_style: *const c_char,
}

#[repr(C)]
#[derive(Default)]
pub struct TtsModelConfig {
    pub vits: VitsModelConfig,
    pub num_threads: c_int,
    pub debug: c_int,
    pub provider: *const c_char,
    pub matcha: MatchaModelConfig,
    pub kokoro: KokoroModelConfig,
    pub kitten: KittenModelConfig,
    pub zipvoice: ZipvoiceModelConfig,
    pub pocket: PocketModelConfig,
    pub supertonic: SupertonicModelConfig,
}

#[repr(C)]
#[derive(Default)]
pub struct TtsConfig {
    pub model: TtsModelConfig,
    pub rule_fsts: *const c_char,
    pub max_num_sentences: c_int,
    pub rule_fars: *const c_char,
    pub silence_scale: f32,
}

#[repr(C)]
pub struct GenerationConfig {
    pub silence_scale: f32,
    pub speed: f32,
    pub sid: c_int,
    pub reference_audio: *const f32,
    pub reference_audio_len: c_int,
    pub reference_sample_rate: c_int,
    pub reference_text: *const c_char,
    pub num_steps: c_int,
    pub extra: *const c_char,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        GenerationConfig {
            silence_scale: 1.0,
            speed: 1.0,
            sid: 0,
            reference_audio: std::ptr::null(),
            reference_audio_len: 0,
            reference_sample_rate: 0,
            reference_text: std::ptr::null(),
            num_steps: 0,
            extra: std::ptr::null(),
        }
    }
}

#[repr(C)]
pub struct GeneratedAudio {
    pub samples: *const f32,
    pub n: c_int,
    pub sample_rate: c_int,
}

pub type FnCreate = unsafe extern "C" fn(*const TtsConfig) -> *const c_void;
pub type FnDestroy = unsafe extern "C" fn(*const c_void);
pub type FnSampleRate = unsafe extern "C" fn(*const c_void) -> c_int;
/// 末尾两个参数是进度回调和它的用户指针，我们一次合成一整段，传 NULL。
pub type FnGenerate = unsafe extern "C" fn(
    *const c_void,
    *const c_char,
    *const GenerationConfig,
    *const c_void,
    *mut c_void,
) -> *const GeneratedAudio;
pub type FnFreeAudio = unsafe extern "C" fn(*const GeneratedAudio);
