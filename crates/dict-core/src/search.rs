//! 打分与归并。
//!
//! ```text
//! score = w_lane · SCALE · freq_pct + exact_bonus − λ·(keyLen − queryLen)
//! ```
//!
//! 三条规则，都是踩过的坑：
//!
//! 1. **词频必须先百分位化。** SUBTLEX 类是「次/百万词」，BNC/COCA 是排名，
//!    量纲完全不同。构建期各自转成 0..1，运行期只比百分位。
//! 2. **exact_bonus 是词典和输入法的分野。** 输入法猜你想打什么，词典用户
//!    通常已经知道要查什么。完整前缀命中必须压过高频词，否则查「事」会被
//!    「时间」一直挡住。
//! 3. **词头命中给满额，释义命中给半额。** 否则打 `china` 会因为「中国」
//!    词频高而永远拿不到英文的 china（瓷器）。

use crate::fasthash::{IdMap, IdSet};
use crate::lane::Lane;
use crate::store::{ids_of, unpack_top, Store, MAX_TOP_PREFIX};

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub scale: f32,
    /// 完整前缀命中的奖励。默认 6.0。
    pub exact_bonus: f32,
    /// 键比查询长出的每个字符扣多少分。
    pub lambda: f32,
    /// 二等（释义）通道的 exact_bonus 折算系数。
    pub secondary: f32,
    pub limit: usize,
    /// 结果里最多留几条二等（释义）命中。
    ///
    /// 只靠 0.5 的权重压不住：释义词元被完整打中时也拿 exact_bonus，
    /// 一个常见义项能同时命中几十个英文词。不封顶的话查「编译」会被
    /// redacted / Aimaco 这类噪音挤掉「编译器」。
    pub max_secondary: usize,
    /// 每通道最多访问的键数，防止一字符查询在几百万词头上退化。
    pub budget: usize,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            scale: 8.0,
            exact_bonus: 6.0,
            lambda: 0.35,
            secondary: 0.5,
            limit: 8,
            max_secondary: 3,
            budget: 250_000,
        }
    }
}

impl Params {
    /// 打分公式的唯一实现。
    ///
    /// 构建期生成短前缀预算表时也调它 —— 两边必须字字一致，否则预算表选出的
    /// 「前 N 名」和运行期的排序不是一回事。
    pub fn score(&self, lane: Lane, key_len: usize, q_len: usize, pct: f32) -> f32 {
        let exact = key_len == q_len && lane.allows_exact_bonus();
        let bonus = if exact {
            self.exact_bonus * if lane.is_secondary() { self.secondary } else { 1.0 }
        } else {
            0.0
        };
        let pen = self.lambda * key_len.saturating_sub(q_len) as f32;
        lane.weight() * self.scale * pct + bonus - pen
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub id: u32,
    pub lane: Lane,
    pub score: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Results {
    pub hits: Vec<Hit>,
    /// 有通道因为超出 budget 被截断。UI 可以据此提示「结果可能不全」。
    pub truncated: bool,
    /// 本次访问的键总数，用于观测。
    pub scanned: usize,
}

pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
}

struct Merger {
    best: IdMap<Hit>,
}

impl Merger {
    fn put(&mut self, id: u32, lane: Lane, key_len: usize, q_len: usize, p: &Params, pct: f32) {
        let score = p.score(lane, key_len, q_len, pct);
        self.best
            .entry(id)
            .and_modify(|h| {
                if score > h.score {
                    *h = Hit { id, lane, score };
                }
            })
            .or_insert(Hit { id, lane, score });
    }
}

impl Store {
    pub fn search(&self, query: &str, p: &Params) -> Results {
        let q = query.trim();
        if q.is_empty() {
            return Results::default();
        }
        let lower = q.to_lowercase();
        let q_len = lower.chars().count();
        let cjk = lower.chars().any(is_cjk);

        let mut m = Merger { best: IdMap::default() };
        let mut scanned = 0usize;
        let mut truncated = false;

        // 含 CJK 时才能确定地关掉英文侧通道；ASCII 输入一律全跑。
        let lanes: &[Lane] = if cjk {
            &[Lane::Zh, Lane::ZhSub, Lane::Trans]
        } else {
            &[Lane::Zh, Lane::Enw, Lane::Py, Lane::Ini, Lane::Gloss]
        };

        for &lane in lanes {
            // 简拼超过 4 个字母基本是误伤；释义路太短会淹没词头路。
            match lane {
                Lane::Ini if q_len > 4 => continue,
                Lane::Gloss if q_len < 3 => continue,
                _ => {}
            }
            if !self.has_lane(lane) {
                continue;
            }

            let pct = &self.pct;

            // 短前缀先看预算表。前缀 `a` 在英文侧要流过九万个键（约 25 ms），
            // fst 逐键重建键字节就是全部开销，绕开它是唯一有效的办法。
            if q_len <= MAX_TOP_PREFIX {
                if let Some(raw) = self.top_postings(lane, &lower) {
                    for v in ids_of(raw) {
                        let (klen, id) = unpack_top(v);
                        let pv = pct.get(id as usize).copied().unwrap_or(0.0);
                        m.put(id, lane, klen, q_len, p, pv);
                    }
                    continue;
                }
            }

            // 边扫边打分，不为每个键分配中间 Vec。
            let seen = self.scan_prefix(lane, &lower, p.budget, |k, raw| {
                let klen = std::str::from_utf8(k).map(|s| s.chars().count()).unwrap_or(k.len());
                for id in ids_of(raw) {
                    let v = pct.get(id as usize).copied().unwrap_or(0.0);
                    m.put(id, lane, klen, q_len, p, v);
                }
            });
            scanned += seen;

            if seen >= p.budget {
                truncated = true;
                // 被截断时单独把精确键捞回来，保证「查什么就出什么」不丢。
                self.exact_key(lane, &lower, |raw| {
                    for id in ids_of(raw) {
                        let v = pct.get(id as usize).copied().unwrap_or(0.0);
                        m.put(id, lane, q_len, q_len, p, v);
                    }
                });
            }
        }

        let mut ranked: Vec<Hit> = m.best.into_values().collect();
        // 同分时按 id 升序，保证结果可复现。
        ranked.sort_by(|a, b| {
            b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then(a.id.cmp(&b.id))
        });

        // 释义路封顶，把剩下的位置留给词头路。
        let mut hits = Vec::with_capacity(p.limit);
        let mut n_secondary = 0usize;
        for h in ranked {
            if hits.len() >= p.limit {
                break;
            }
            if h.lane.is_secondary() {
                if n_secondary >= p.max_secondary {
                    continue;
                }
                n_secondary += 1;
            }
            hits.push(h);
        }
        Results { hits, truncated, scanned }
    }
}

/// 稳定排序：query 是上一次的延长时**只过滤不重排**。
///
/// 用户眼睛已经锁定第 3 项时它跳到第 1 项，比慢更烦人。
/// 前缀扩展时匹配集是子集，过滤天然保序。
pub fn stabilize(prev: &[Hit], fresh: Vec<Hit>) -> Vec<Hit> {
    let keep: IdMap<Hit> = fresh.iter().map(|h| (h.id, *h)).collect();
    let mut out: Vec<Hit> = prev.iter().filter_map(|h| keep.get(&h.id).copied()).collect();
    let seen: IdSet = out.iter().map(|h| h.id).collect();
    out.extend(fresh.into_iter().filter(|h| !seen.contains(&h.id)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: u32, score: f32) -> Hit {
        Hit { id, lane: Lane::Zh, score }
    }

    #[test]
    fn stabilize_keeps_previous_order() {
        let prev = vec![hit(10, 5.0), hit(20, 4.0), hit(30, 3.0)];
        // 新一轮里 30 反超到第一，但它在上一轮已经出现过 → 保持原位。
        let fresh = vec![hit(30, 9.0), hit(10, 5.0), hit(40, 1.0)];
        let out = stabilize(&prev, fresh);
        assert_eq!(out.iter().map(|h| h.id).collect::<Vec<_>>(), vec![10, 30, 40]);
        // 分数用新的，位置用旧的
        assert_eq!(out[1].score, 9.0);
    }

    #[test]
    fn cjk_detection() {
        assert!(is_cjk('中'));
        assert!(is_cjk('龙'));
        assert!(!is_cjk('a'));
        assert!(!is_cjk('ā'));
    }
}
