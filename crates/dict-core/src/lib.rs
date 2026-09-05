//! 本地中英词典的核心：数据模型、只读 mmap 词库、多路检索与打分。
//!
//! 中文词和英文词都是一等词头，各有独立词条页。构建期把
//! CC-CEDICT / ECDICT / Tatoeba / Unihan / jieba 词频归一成同一套结构，
//! 运行期纯查表。

pub mod dynlib;
pub mod fasthash;
pub mod lane;
pub mod model;
pub mod pinyin;
pub mod search;
pub mod store;

pub use lane::{Lane, LANES};
pub use model::{Entry, Example, Sense, KIND_EN, KIND_ZH};
pub use search::{is_cjk, stabilize, Hit, Params, Results};
pub use store::{Store, StoreWriter};

/// 把一组原始词频转成本语料内的百分位 0..1（1 = 最常用）。
///
/// `bigger_is_more_common` 对「次/百万词」是 true，对 BNC/COCA 排名是 false。
/// 未知词频（`None`）一律排在最后，彼此并列。
pub fn percentiles(freqs: &[Option<f64>], bigger_is_more_common: bool) -> Vec<f32> {
    let n = freqs.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        let (fa, fb) = (freqs[a], freqs[b]);
        match (fa, fb) {
            (Some(x), Some(y)) => {
                let c = x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
                if bigger_is_more_common {
                    c.reverse()
                } else {
                    c
                }
            }
            // 已知词频一律排在未知之前
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then(a.cmp(&b))
    });

    let known = freqs.iter().filter(|f| f.is_some()).count();
    let denom = known.saturating_sub(1).max(1) as f32;
    let mut out = vec![0.0f32; n];
    for (rank, &i) in order.iter().enumerate() {
        // 排序已把已知词频全部放在前 `known` 位，所以 rank 就是它在已知集里的名次。
        out[i] = if freqs[i].is_none() {
            // 未知词频压到所有已知词频之下，但不给 0，保留一点区分度。
            0.02
        } else {
            // 已知部分铺满 0.10 .. 1.00
            (0.10 + 0.90 * (1.0 - rank as f32 / denom)).clamp(0.10, 1.0)
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_orders_by_commonness() {
        // 次/百万词：越大越常用
        let p = percentiles(&[Some(1.0), Some(100.0), Some(10.0)], true);
        assert!(p[1] > p[2] && p[2] > p[0]);

        // 排名：越小越常用
        let p = percentiles(&[Some(1.0), Some(100.0), Some(10.0)], false);
        assert!(p[0] > p[2] && p[2] > p[1]);
    }

    #[test]
    fn unknown_frequency_sinks_to_the_bottom() {
        let p = percentiles(&[Some(5.0), None, Some(1.0)], true);
        assert!(p[1] < p[2], "未知词频要排在所有已知词频之后");
        assert!(p[0] > p[2]);
    }
}
