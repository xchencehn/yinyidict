//! 找运行库和 CUDA 依赖。
//!
//! 目录名带版本号，只能扫不能拼。cuDNN 和 CUDA Toolkit 都把 DLL 放在
//! `bin/x64/` 而不是 `bin/`，两处都要看。

use std::path::{Path, PathBuf};

/// 判断一个路径是不是 CUDA 版的运行库。
pub fn is_cuda(p: &Path) -> bool {
    p.to_string_lossy().to_lowercase().contains("cuda")
}

/// 扫一个 vendor 目录下所有可用的 sherpa 运行库（含 `sherpa-onnx-c-api.dll` 的 `lib/`）。
pub fn sherpa_libs(vendor: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(vendor)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("lib"))
        .filter(|p| p.join("sherpa-onnx-c-api.dll").exists())
        .collect();
    v.sort();
    v
}

/// 某个目录下放 DLL 的实际位置：优先 `bin/x64`，退回 `bin`。
fn dll_bin(dir: &Path, probe: &str) -> Option<PathBuf> {
    for sub in ["bin/x64", "bin"] {
        let b = dir.join(sub);
        if b.join(probe).exists() {
            return Some(b);
        }
    }
    None
}

/// `onnxruntime_providers_cuda.dll` 会按名字去找 cudnn64_9 / cublas64_13 等等，
/// 它们分散在 cuDNN 包和 CUDA Toolkit 各自的 bin 下。把这两处都找出来。
///
/// 两样缺一就返回空 —— 调用方据此判断能不能走 GPU。
pub fn cuda_dll_dirs(vendor_cuda: &Path) -> Vec<PathBuf> {
    let cudnn = std::fs::read_dir(vendor_cuda)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .find_map(|e| dll_bin(&e.path(), "cudnn64_9.dll"));

    let toolkit = {
        let root = Path::new(r"C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA");
        let mut v: Vec<PathBuf> = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter_map(|e| dll_bin(&e.path(), "cudart64_13.dll"))
            .collect();
        v.sort();
        v.pop()
    };

    match (cudnn, toolkit) {
        (Some(a), Some(b)) => vec![a, b],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_cuda_paths() {
        assert!(is_cuda(Path::new("vendor/sherpa-onnx-v1-cuda-13.x/lib")));
        assert!(!is_cuda(Path::new("vendor/sherpa-onnx-v1-win-x64-shared/lib")));
    }

    #[test]
    fn missing_directories_yield_nothing_rather_than_panicking() {
        assert!(sherpa_libs(Path::new("definitely/not/here")).is_empty());
        assert!(cuda_dll_dirs(Path::new("definitely/not/here")).is_empty());
    }
}
