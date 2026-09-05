// 图标的像素生成。**纯计算，不依赖任何 crate** ——
// `build.rs` 要 `include!` 这个文件来生成嵌进 exe 的 .ico，
// 那时候还没有 egui 可用。
//
// 设计取自界面本身：玫瑰底（声调一的颜色，全局唯一的强调色）+ 米色横线，
// 像一页有一行被标出来的文字。16 像素下要认得出，所以只有块面，没有细节。

/// 生成 `size × size` 的 RGBA 图标。
pub fn rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];

    // 调色板直接取自 theme.rs，保持同一套语言
    const ROSE: [u8; 3] = [0xB0, 0x3A, 0x48]; // 声调一
    const CREAM: [u8; 3] = [0xFB, 0xFA, 0xF8];

    let r = s * 0.22; // 圆角半径
    let inset = s * 0.06;
    let (lo, hi) = (inset, s - inset);

    // 三条「文字行」：上两条米色，中间偏下那条更短，像段落收尾
    let lines = [
        (0.30f32, 0.22f32, 0.62f32, 1.0f32),
        (0.48, 0.22, 0.62, 1.0),
        (0.66, 0.22, 0.44, 1.0),
    ];
    let line_h = (s * 0.075).max(1.0);

    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // 圆角矩形的覆盖率，边缘做一像素抗锯齿
            let cov = rounded_rect_coverage(fx, fy, lo, hi, r);
            if cov <= 0.0 {
                continue;
            }

            let mut col = ROSE;
            let mut on_line = 0.0f32;
            for &(cy, cx0, cw, alpha) in &lines {
                let y0 = lo + (hi - lo) * cy;
                let x0 = lo + (hi - lo) * cx0;
                let x1 = x0 + (hi - lo) * cw;
                if fy >= y0 && fy < y0 + line_h && fx >= x0 && fx < x1 {
                    on_line = on_line.max(alpha);
                }
            }
            if on_line > 0.0 {
                col = CREAM;
            }

            let i = ((y * size + x) * 4) as usize;
            px[i] = col[0];
            px[i + 1] = col[1];
            px[i + 2] = col[2];
            px[i + 3] = (cov * 255.0) as u8;
        }
    }
    px
}

/// 点 (x,y) 落在圆角矩形内的比例，0..1。边缘按到圆角圆心的距离做一像素过渡。
fn rounded_rect_coverage(x: f32, y: f32, lo: f32, hi: f32, r: f32) -> f32 {
    if x < lo - 1.0 || x > hi + 1.0 || y < lo - 1.0 || y > hi + 1.0 {
        return 0.0;
    }
    // 把点夹到「圆角圆心构成的内矩形」上，算它到内矩形的距离
    let cx = x.clamp(lo + r, hi - r);
    let cy = y.clamp(lo + r, hi - r);
    let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
    (r - d + 0.5).clamp(0.0, 1.0)
}
