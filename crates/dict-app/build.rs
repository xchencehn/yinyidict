//! 把程序图标嵌进 exe 的资源段。
//!
//! **为什么非做不可**：桌面快捷方式、资源管理器和任务栏用的都是 exe 里**嵌入的
//! 图标资源**，跟运行期发的 `WM_SETICON` 和托盘的 HICON 完全是两回事。
//! 只做后两者的话，窗口标题栏有图标，快捷方式却还是一个白板。
//!
//! 图标在这里现画（`icon_draw.rs` 是纯计算，不依赖任何 crate），写成多尺寸 .ico，
//! 再交给 MinGW 的 windres 编成 COFF 对象链进去 —— 不往版本库里塞二进制资源。
//!
//! 找不到 windres 时只警告不失败：图标是锦上添花，不该挡住构建。

include!("src/icon_draw.rs");

use std::path::{Path, PathBuf};

/// 要放进 .ico 的尺寸。16/32 是列表和任务栏，48 是大图标视图，256 是超大图标。
const SIZES: &[u32] = &[16, 20, 24, 32, 48, 64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=src/icon_draw.rs");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("dict.ico");
    if let Err(e) = std::fs::write(&ico, build_ico()) {
        println!("cargo:warning=写 .ico 失败：{e}");
        return;
    }

    let Some(windres) = find_windres() else {
        println!("cargo:warning=找不到 windres，exe 不会带图标资源（快捷方式会是白板）");
        return;
    };

    // 资源 id 用 1：Windows 拿编号最小的那个图标当程序图标
    let rc = out.join("dict.rc");
    let rc_text = format!("1 ICON \"{}\"\n", ico.display().to_string().replace('\\', "/"));
    if let Err(e) = std::fs::write(&rc, rc_text) {
        println!("cargo:warning=写 .rc 失败：{e}");
        return;
    }

    let obj = out.join("dict-icon.o");
    match std::process::Command::new(&windres)
        .args(["-i", rc.to_str().unwrap_or_default()])
        .args(["-o", obj.to_str().unwrap_or_default()])
        .args(["-O", "coff"])
        .output()
    {
        Ok(o) if o.status.success() => {
            println!("cargo:rustc-link-arg-bins={}", obj.display());
        }
        Ok(o) => {
            println!("cargo:warning=windres 失败：{}", String::from_utf8_lossy(&o.stderr).trim())
        }
        Err(e) => println!("cargo:warning=windres 起不来：{e}"),
    }
}

/// 优先用工程自带的那份 MinGW（`.cargo/config.toml` 里也指着它），
/// 其次碰运气看 PATH。
fn find_windres() -> Option<PathBuf> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").ok()?);
    let local = manifest.join("../../.bin/mingw64/bin/windres.exe");
    if local.exists() {
        return Some(local);
    }
    let path = Path::new("windres.exe");
    std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| path.to_path_buf())
}

/// 组装多尺寸 .ico。
///
/// 每张图是「BITMAPINFOHEADER + 32 位 BGRA 色数据 + 1 位 AND 掩码」，
/// 而且**自下而上**存 —— DIB 的老规矩。头里的高度要写两倍，因为它把掩码
/// 也算进同一张位图里。掩码全 0（全不透明），透明度交给 alpha 通道。
fn build_ico() -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = SIZES.iter().map(|&s| (s, dib_for(s))).collect();

    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type = icon
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());

    // 目录项之后紧跟着图像数据
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, data) in &images {
        // 256 在这个字段里记作 0
        let byte = if *size >= 256 { 0u8 } else { *size as u8 };
        out.push(byte); // width
        out.push(byte); // height
        out.push(0); // 调色板色数，32 位图为 0
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &images {
        out.extend_from_slice(data);
    }
    out
}

fn dib_for(size: u32) -> Vec<u8> {
    let px = rgba(size);
    let w = size as i32;
    let h = size as i32;
    // 掩码每行按 4 字节对齐
    let mask_stride = (size.div_ceil(32) * 4) as usize;

    let mut d = Vec::with_capacity(40 + px.len() + mask_stride * size as usize);
    d.extend_from_slice(&40u32.to_le_bytes()); // biSize
    d.extend_from_slice(&w.to_le_bytes());
    d.extend_from_slice(&(h * 2).to_le_bytes()); // 色图 + 掩码，所以是两倍
    d.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    d.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    d.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    d.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
    for _ in 0..4 {
        d.extend_from_slice(&0u32.to_le_bytes()); // 分辨率和调色板计数
    }

    // 色数据：BGRA，自下而上
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            d.push(px[i + 2]); // B
            d.push(px[i + 1]); // G
            d.push(px[i]); // R
            d.push(px[i + 3]); // A
        }
    }
    // AND 掩码：全 0 = 全部「不透明」，实际透明度由 alpha 决定
    d.extend(std::iter::repeat_n(0u8, mask_stride * size as usize));
    d
}
