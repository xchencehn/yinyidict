//! 给「u32 词条 id」用的整数哈希。
//!
//! 归并阶段一次查询要往表里塞十万条（单字符前缀能命中九万个键），
//! 标准库默认的 SipHash 在这个量级上就是主要开销。这里的键是我们自己
//! 分配的稠密 id，不来自外部输入，不需要抗 HashDoS。
//!
//! 用的是 Fibonacci hashing：乘上 2^64/φ 再取高位，把低位的规律性摊到整个字长。

use std::hash::{BuildHasherDefault, Hasher};

/// 2^64 / φ，取奇数。
const K: u64 = 0x9E37_79B9_7F4A_7C15;

#[derive(Default, Clone, Copy)]
pub struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // 兜底路径：本模块只打算给整数键用，但 Hasher 契约要求它能工作。
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(K);
        }
    }

    fn write_u32(&mut self, n: u32) {
        self.0 = (n as u64).wrapping_mul(K);
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = n.wrapping_mul(K);
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
}

pub type IdBuildHasher = BuildHasherDefault<IdHasher>;
pub type IdMap<V> = std::collections::HashMap<u32, V, IdBuildHasher>;
pub type IdSet = std::collections::HashSet<u32, IdBuildHasher>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_behaves_like_a_normal_hashmap() {
        let mut m: IdMap<&str> = IdMap::default();
        for i in 0..10_000u32 {
            m.insert(i, "x");
        }
        assert_eq!(m.len(), 10_000);
        assert_eq!(m.get(&5_000), Some(&"x"));
        assert_eq!(m.get(&10_001), None);
        m.remove(&5_000);
        assert_eq!(m.get(&5_000), None);
    }

    #[test]
    fn distinct_ids_spread_across_buckets() {
        // 连续 id 乘上黄金比例常数后，高位应当分散开而不是扎堆。
        let h = |n: u32| {
            let mut x = IdHasher::default();
            x.write_u32(n);
            x.finish() >> 56
        };
        let distinct: std::collections::HashSet<u64> = (0..256u32).map(h).collect();
        assert!(distinct.len() > 200, "只散出 {} 个桶", distinct.len());
    }
}
