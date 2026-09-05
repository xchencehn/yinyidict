//! 播放与波形处理。
//!
//! 播放走 winmm 的 `PlaySoundW` + `SND_MEMORY`：一次调用，异步，自己管设备，
//! 新的一次播放自动打断上一次 —— 正是词典要的行为。
//!
//! 为什么不用 cpal：它经 windows-sys 走 raw-dylib，在 windows-gnu 工具链上
//! 需要 dlltool，本机没有。

use dict_core::dynlib::DynLib;
use std::ffi::c_void;

const SND_ASYNC: u32 = 0x0001;
const SND_MEMORY: u32 = 0x0004;
const SND_NODEFAULT: u32 = 0x0002;

type FnPlaySound = unsafe extern "system" fn(*const u8, *mut c_void, u32) -> i32;

/// 一个 winmm 播放器。持有正在播放的缓冲区 —— `SND_ASYNC` 期间那段内存必须一直有效。
pub struct Player {
    lib: DynLib,
    play: FnPlaySound,
    /// 正在播放的 WAV。换新的之前必须先停掉旧的，否则 winmm 还在读已释放的内存。
    current: Option<Box<[u8]>>,
}

impl Player {
    pub fn new() -> anyhow::Result<Self> {
        let lib = DynLib::open("winmm.dll")?;
        // SAFETY: PlaySoundW 的签名取自 Win32 文档，winmm 是系统组件。
        let play: FnPlaySound = unsafe { lib.sym("PlaySoundW")? };
        Ok(Player { lib, play, current: None })
    }

    pub fn stop(&mut self) {
        // SAFETY: 传 NULL 表示停止当前播放，是 PlaySound 的既定用法。
        unsafe {
            (self.play)(std::ptr::null(), std::ptr::null_mut(), 0);
        }
        self.current = None;
    }

    /// 播放一段内存里的 WAV。会打断上一次播放。
    pub fn play_wav(&mut self, wav: Vec<u8>) -> anyhow::Result<()> {
        // 先停：winmm 可能还在读旧缓冲区，此时释放它就是释放后使用。
        self.stop();
        let buf = wav.into_boxed_slice();
        let ptr = buf.as_ptr();
        self.current = Some(buf);
        // SAFETY: buf 由 self.current 持有，在下一次 stop() 之前不会被释放；
        // SND_MEMORY 要求指针指向一段完整的 WAV 映像，正是这里传的内容。
        let rc = unsafe {
            (self.play)(ptr, std::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT)
        };
        if rc == 0 {
            anyhow::bail!("PlaySoundW 返回失败");
        }
        let _ = &self.lib;
        Ok(())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

/// f32 单声道样本 → 16 位 PCM 的 WAV 映像。
pub fn to_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let n = samples.len();
    let data_len = (n * 2) as u32;
    let mut out = Vec::with_capacity(44 + n * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt 块长度
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // 单声道
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // 字节率
    out.extend_from_slice(&2u16.to_le_bytes()); // 块对齐
    out.extend_from_slice(&16u16.to_le_bytes()); // 位深
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_is_well_formed() {
        let w = to_wav(&[0.0, 1.0, -1.0], 24000);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        assert_eq!(&w[36..40], b"data");
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 24000);
        // 满幅样本要落在 i16 的边界上
        assert_eq!(i16::from_le_bytes(w[46..48].try_into().unwrap()), i16::MAX);
        assert_eq!(i16::from_le_bytes(w[48..50].try_into().unwrap()), -i16::MAX);
    }
}
