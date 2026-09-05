//! 程序图标。
//!
//! 像素怎么画在 [`icon_draw`](../icon_draw.rs) 里 —— 那份代码不依赖任何 crate，
//! 因为 `build.rs` 也要用它生成嵌进 exe 的 .ico。
//!
//! 三个地方要用图标，来源各不相同，缺一个就少一处：
//! - **标题栏 / Alt+Tab**：运行期发 `WM_SETICON`
//! - **托盘**：`Shell_NotifyIcon` 带的 HICON
//! - **桌面快捷方式 / 资源管理器 / 任务栏**：**exe 里嵌入的图标资源**，
//!   跟前两者完全无关，只能在构建期塞进去

include!("icon_draw.rs");

/// eframe 的窗口图标。
pub fn window_icon() -> egui::IconData {
    let size = 64;
    egui::IconData { rgba: rgba(size), width: size, height: size }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(px: &[u8], size: u32, x: u32, y: u32) -> u8 {
        px[((y * size + x) * 4 + 3) as usize]
    }

    #[test]
    fn corners_are_transparent_and_middle_is_opaque() {
        let size = 32;
        let px = rgba(size);
        assert_eq!(px.len(), (size * size * 4) as usize);
        assert_eq!(alpha_at(&px, size, 0, 0), 0, "圆角外应当透明");
        assert_eq!(alpha_at(&px, size, size - 1, 0), 0);
        assert_eq!(alpha_at(&px, size, size / 2, size / 2), 255, "中心应当不透明");
    }

    #[test]
    fn the_mark_reads_at_tray_size() {
        // 16 像素下仍要有可辨识的明暗对比，否则托盘里就是一团色块
        let size = 16;
        let px = rgba(size);
        let mut cream = 0;
        let mut rose = 0;
        for i in (0..px.len()).step_by(4) {
            if px[i + 3] < 128 {
                continue;
            }
            if px[i] > 200 {
                cream += 1;
            } else {
                rose += 1;
            }
        }
        assert!(cream > 8, "米色横线太少（{cream} 像素），16px 下认不出");
        assert!(rose > cream, "底色应当仍是主体（玫瑰 {rose} / 米色 {cream}）");
    }

    #[test]
    fn every_size_is_well_formed() {
        for size in [16u32, 20, 24, 32, 48, 64, 256] {
            let px = rgba(size);
            assert_eq!(px.len(), (size * size * 4) as usize, "size={size}");
            assert!(px.chunks_exact(4).any(|p| p[3] == 255), "size={size} 全透明了");
        }
    }
}
