//! 只读 mmap 词库。
//!
//! 运行期不做任何解析：候选打分只碰 `pct` / `kind` 两个内存数组，
//! 只有真正要显示的那一条才从 mmap 反序列化词条正文。

use crate::lane::{Lane, LANES};
use crate::model::{Entry, Sense};
use anyhow::{bail, Context, Result};
use fst::automaton::Str;
use fst::{Automaton, IntoStreamer, Streamer};
use memmap2::Mmap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"ZDICT001";

fn map_file(path: &Path) -> Result<Mmap> {
    let f = File::open(path).with_context(|| format!("打不开 {}", path.display()))?;
    // SAFETY: 词库由构建期一次性生成，运行期只读、无并发改写。
    Ok(unsafe { Mmap::map(&f)? })
}

fn rd_u32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn rd_u64(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

/// 把 postings 的字节切片解成 id 序列。
pub fn ids_of(raw: &[u8]) -> impl Iterator<Item = u32> + '_ {
    raw.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap()))
}

/// 短前缀预算表覆盖到几个字符。
pub const MAX_TOP_PREFIX: usize = 2;

/// 预算表的 postings 把「键长」和「词条 id」打包进一个 u32：
/// 高 8 位是键长，低 24 位是 id。词条数远小于 2^24，够用。
pub fn pack_top(key_len: usize, id: u32) -> u32 {
    debug_assert!(id < (1 << 24), "词条 id {id} 超过预算表能表示的 24 位");
    ((key_len.min(255) as u32) << 24) | (id & 0x00FF_FFFF)
}

/// `pack_top` 的逆操作，返回 `(键长, id)`。
pub fn unpack_top(v: u32) -> (usize, u32) {
    ((v >> 24) as usize, v & 0x00FF_FFFF)
}

struct LaneIndex {
    fst: fst::Map<Mmap>,
    post: Mmap,
}

impl LaneIndex {
    /// fst 的值是本通道 postings 文件里的字节偏移，那里存 `[u32 count][u32 id]*`。
    fn ids_raw(&self, off: u64) -> &[u8] {
        let o = off as usize;
        let n = rd_u32(&self.post, o) as usize;
        &self.post[o + 4..o + 4 + n * 4]
    }
}

/// 义项重组旁路表：`llm.idx` 是 `[u64; n+1]` 偏移，长度为 0 表示这条没有重组结果。
struct Overlay {
    bin: Mmap,
    idx: Mmap,
}

impl Overlay {
    fn range(&self, id: u32) -> Option<(usize, usize)> {
        let i = id as usize;
        if (i + 2) * 8 > self.idx.len() {
            return None;
        }
        let a = rd_u64(&self.idx, i * 8) as usize;
        let b = rd_u64(&self.idx, (i + 1) * 8) as usize;
        (b > a && b <= self.bin.len()).then_some((a, b))
    }

    fn has(&self, id: u32) -> bool {
        self.range(id).is_some()
    }

    fn senses(&self, id: u32) -> Option<Vec<Sense>> {
        let (a, b) = self.range(id)?;
        bincode::deserialize(&self.bin[a..b]).ok()
    }

    fn count(&self) -> usize {
        let n = self.idx.len() / 8;
        (0..n.saturating_sub(1)).filter(|&i| self.has(i as u32)).count()
    }
}

pub struct Store {
    pub n: usize,
    /// 本语料内百分位 0..1。跨语料打分只比这个值 —— 中文侧是「次/百万词」、
    /// 英文侧是 BNC 排名，量纲完全不同，直接进同一个打分函数会得出胡话。
    pub pct: Vec<f32>,
    pub kind: Vec<u8>,
    entries: Mmap,
    idx: Mmap,
    /// 义项重组的旁路表。跟主索引分开存：重跑 ETL 不该丢掉它，
    /// 换模型或改 prompt 也能单独重来。没有这个文件时一切照旧。
    overlay: Option<Overlay>,
    lanes: Vec<Option<LaneIndex>>,
    /// 短前缀预算表。构建期只给候选量大的前缀建，正好是扫描慢的那些。
    tops: Vec<Option<LaneIndex>>,
}

impl Store {
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(dir.as_ref(), true)
    }

    /// 打开词库但**不加载义项重组旁路表**。
    ///
    /// 构建期一律用这个。重组流水线的每一步都要拿**原始**义项做基准 ——
    /// 导出时判断义项够不够多、导入时校验 `from` 下标是否越界 ——
    /// 一旦读到上一轮重组过的版本，就会越跑越偏，而且是静默的。
    /// 这个坑先后在 `import` 和 `export` 各犯过一次，所以干脆从入口就断掉。
    pub fn open_raw(dir: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(dir.as_ref(), false)
    }

    fn open_inner(dir: &Path, with_overlay: bool) -> Result<Self> {
        let meta = map_file(&dir.join("meta.bin"))?;
        if meta.len() < 12 || &meta[..8] != MAGIC {
            bail!("{} 不是本程序的词库（magic 不匹配）", dir.display());
        }
        let n = rd_u32(&meta, 8) as usize;
        let pct_off = 12;
        let kind_off = pct_off + n * 4;
        if meta.len() < kind_off + n {
            bail!("meta.bin 截断：声明 {n} 条，实际只有 {} 字节", meta.len());
        }
        let pct = (0..n)
            .map(|i| {
                f32::from_le_bytes(meta[pct_off + i * 4..pct_off + i * 4 + 4].try_into().unwrap())
            })
            .collect();
        let kind = meta[kind_off..kind_off + n].to_vec();

        let load = |stem: &str| -> Result<Option<LaneIndex>> {
            let fp = dir.join(format!("{stem}.fst"));
            let pp = dir.join(format!("{stem}.post"));
            Ok(if fp.exists() && pp.exists() {
                Some(LaneIndex { fst: fst::Map::new(map_file(&fp)?)?, post: map_file(&pp)? })
            } else {
                None
            })
        };

        let mut lanes = Vec::with_capacity(LANES.len());
        let mut tops = Vec::with_capacity(LANES.len());
        for lane in LANES {
            lanes.push(load(lane.file_stem())?);
            tops.push(load(&format!("{}top", lane.file_stem()))?);
        }

        let ov = dir.join("llm.bin");
        let ovi = dir.join("llm.idx");
        let overlay = if with_overlay && ov.exists() && ovi.exists() {
            Some(Overlay { bin: map_file(&ov)?, idx: map_file(&ovi)? })
        } else {
            None
        };

        Ok(Store {
            n,
            pct,
            kind,
            entries: map_file(&dir.join("entries.bin"))?,
            idx: map_file(&dir.join("entries.idx"))?,
            overlay,
            lanes,
            tops,
        })
    }

    /// 反序列化一条词条正文。只有要显示的那条才走这里。
    ///
    /// 有重组结果时用它替换义项；原始义项仍在 `raw` 里，词条页照常能对照。
    pub fn entry(&self, id: u32) -> Result<Entry> {
        let i = id as usize;
        if i >= self.n {
            bail!("词条 id {i} 越界（共 {} 条）", self.n);
        }
        let a = rd_u64(&self.idx, i * 8) as usize;
        let b = rd_u64(&self.idx, (i + 1) * 8) as usize;
        let mut e: Entry = bincode::deserialize(&self.entries[a..b])?;
        if let Some(senses) = self.overlay.as_ref().and_then(|o| o.senses(id)) {
            e.senses = senses;
        }
        Ok(e)
    }

    /// 读原始词条，**不套旁路表**。
    ///
    /// 重组结果的接地校验必须用它：校验要拿 `from` 下标去比对**原始**义项数，
    /// 若用 `entry()` 就会拿到上一轮已经重组过（义项更少）的版本，
    /// 把合法结果误判成越界 —— 而且是越导入越严重的自我毒化。
    pub fn entry_raw(&self, id: u32) -> Result<Entry> {
        let i = id as usize;
        if i >= self.n {
            bail!("词条 id {i} 越界（共 {} 条）", self.n);
        }
        let a = rd_u64(&self.idx, i * 8) as usize;
        let b = rd_u64(&self.idx, (i + 1) * 8) as usize;
        Ok(bincode::deserialize(&self.entries[a..b])?)
    }

    /// 这条词条的义项是不是被重组过。词条页据此提示可以对照原文。
    pub fn is_rewritten(&self, id: u32) -> bool {
        self.overlay.as_ref().is_some_and(|o| o.has(id))
    }

    /// 旁路表里有多少条。
    pub fn overlay_len(&self) -> usize {
        self.overlay.as_ref().map(|o| o.count()).unwrap_or(0)
    }

    pub fn has_lane(&self, lane: Lane) -> bool {
        self.lanes[lane.idx()].is_some()
    }

    /// 遍历某通道下所有以 `prefix` 开头的键。
    ///
    /// `budget` 限制访问的键数量，防止一字符查询在几百万词头上退化。
    /// 返回值是实际访问的键数，等于 budget 说明结果被截断了。
    pub fn scan_prefix(
        &self,
        lane: Lane,
        prefix: &str,
        budget: usize,
        mut f: impl FnMut(&[u8], &[u8]),
    ) -> usize {
        let Some(li) = self.lanes[lane.idx()].as_ref() else {
            return 0;
        };
        let mut stream = li.fst.search(Str::new(prefix).starts_with()).into_stream();
        let mut seen = 0usize;
        while let Some((k, v)) = stream.next() {
            f(k, li.ids_raw(v));
            seen += 1;
            if seen >= budget {
                break;
            }
        }
        seen
    }

    /// 取短前缀的预算结果（`pack_top` 编码）。没有预算表就返回 `None`，
    /// 调用方回落到 `scan_prefix`。
    pub fn top_postings(&self, lane: Lane, prefix: &str) -> Option<&[u8]> {
        let li = self.tops[lane.idx()].as_ref()?;
        let off = li.fst.get(prefix.as_bytes())?;
        Some(li.ids_raw(off))
    }

    /// 按词形精确找一条词头，中文侧优先。词条页里的「对应词条」跳转用它。
    pub fn lookup_word(&self, word: &str) -> Option<u32> {
        let key = word.trim().to_lowercase();
        for lane in [Lane::Zh, Lane::Enw] {
            let mut best: Option<u32> = None;
            self.exact_key(lane, &key, |raw| {
                // 同一词形可能有多条（不同读音），取百分位最高的那条
                best = ids_of(raw).max_by(|a, b| {
                    let (pa, pb) = (self.pct[*a as usize], self.pct[*b as usize]);
                    pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
                });
            });
            if best.is_some() {
                return best;
            }
        }
        None
    }

    /// 精确取一个键的 postings。短查询被 budget 截断时用它兜底，保证精确命中不丢。
    pub fn exact_key(&self, lane: Lane, key: &str, mut f: impl FnMut(&[u8])) {
        let Some(li) = self.lanes[lane.idx()].as_ref() else {
            return;
        };
        if let Some(off) = li.fst.get(key.as_bytes()) {
            f(li.ids_raw(off));
        }
    }
}

// ─────────────────────────── 构建期写入 ───────────────────────────

pub struct StoreWriter {
    dir: PathBuf,
    entries: BufWriter<File>,
    offsets: Vec<u64>,
    pct: Vec<f32>,
    kind: Vec<u8>,
    cursor: u64,
}

impl StoreWriter {
    pub fn create(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(StoreWriter {
            entries: BufWriter::new(File::create(dir.join("entries.bin"))?),
            dir,
            offsets: vec![0],
            pct: Vec::new(),
            kind: Vec::new(),
            cursor: 0,
        })
    }

    pub fn push(&mut self, e: &Entry) -> Result<u32> {
        let bytes = bincode::serialize(e)?;
        self.entries.write_all(&bytes)?;
        self.cursor += bytes.len() as u64;
        self.offsets.push(self.cursor);
        self.pct.push(e.pct);
        self.kind.push(e.kind);
        Ok((self.offsets.len() - 2) as u32)
    }

    pub fn len(&self) -> usize {
        self.pct.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pct.is_empty()
    }

    /// 写一个通道。`keys` 必须按字典序排好且键唯一 —— fst 的构建要求。
    pub fn write_lane(&self, lane: Lane, keys: &[(String, Vec<u32>)]) -> Result<()> {
        self.write_index(lane.file_stem(), keys)
    }

    /// 写一个通道的短前缀预算表，值用 `pack_top` 编码。
    pub fn write_top_lane(&self, lane: Lane, keys: &[(String, Vec<u32>)]) -> Result<()> {
        self.write_index(&format!("{}top", lane.file_stem()), keys)
    }

    fn write_index(&self, stem: &str, keys: &[(String, Vec<u32>)]) -> Result<()> {
        let mut post = BufWriter::new(File::create(self.dir.join(format!("{stem}.post")))?);
        let mut builder = fst::MapBuilder::new(BufWriter::new(File::create(
            self.dir.join(format!("{stem}.fst")),
        )?))?;
        let mut off: u64 = 0;
        for (k, ids) in keys {
            builder
                .insert(k.as_bytes(), off)
                .with_context(|| format!("索引 {stem} 插入键 {k:?} 失败"))?;
            post.write_all(&(ids.len() as u32).to_le_bytes())?;
            for id in ids {
                post.write_all(&id.to_le_bytes())?;
            }
            off += 4 + 4 * ids.len() as u64;
        }
        builder.into_inner()?.flush()?;
        post.flush()?;
        Ok(())
    }

    /// 写义项重组旁路表。`items` 按 id 升序，缺的条目留空。
    pub fn write_overlay(
        dir: impl AsRef<Path>,
        n: usize,
        items: &[(u32, Vec<Sense>)],
    ) -> Result<()> {
        let dir = dir.as_ref();
        let mut bin = BufWriter::new(File::create(dir.join("llm.bin"))?);
        let mut offsets = vec![0u64; n + 1];
        let mut cursor = 0u64;
        let mut it = items.iter().peekable();
        for i in 0..n {
            offsets[i] = cursor;
            if let Some((id, senses)) = it.peek() {
                if *id as usize == i {
                    let bytes = bincode::serialize(senses)?;
                    bin.write_all(&bytes)?;
                    cursor += bytes.len() as u64;
                    it.next();
                }
            }
            offsets[i + 1] = cursor;
        }
        bin.flush()?;

        let mut idx = BufWriter::new(File::create(dir.join("llm.idx"))?);
        for o in &offsets {
            idx.write_all(&o.to_le_bytes())?;
        }
        idx.flush()?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.entries.flush()?;

        let mut idx = BufWriter::new(File::create(self.dir.join("entries.idx"))?);
        for o in &self.offsets {
            idx.write_all(&o.to_le_bytes())?;
        }
        idx.flush()?;

        let mut meta = BufWriter::new(File::create(self.dir.join("meta.bin"))?);
        meta.write_all(MAGIC)?;
        meta.write_all(&(self.pct.len() as u32).to_le_bytes())?;
        for p in &self.pct {
            meta.write_all(&p.to_le_bytes())?;
        }
        meta.write_all(&self.kind)?;
        meta.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Sense, KIND_ZH};

    /// 在临时目录里搭一个最小词库：两条词条，各三个义项。
    fn tiny_store(dir: &Path) -> Result<()> {
        let mut w = StoreWriter::create(dir)?;
        for word in ["甲", "乙"] {
            let e = Entry {
                kind: KIND_ZH,
                word: word.into(),
                senses: (0..3)
                    .map(|i| Sense { text: format!("{word}-原始{i}"), ..Default::default() })
                    .collect(),
                ..Default::default()
            };
            w.push(&e)?;
        }
        w.finish()
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dictcore-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn overlay_replaces_senses_but_entry_raw_does_not_see_it() {
        let dir = tmp("overlay");
        tiny_store(&dir).unwrap();

        // 没有旁路表时，两条路径应当一致
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.entry(0).unwrap().senses.len(), 3);
        assert_eq!(s.entry_raw(0).unwrap().senses.len(), 3);
        assert!(!s.is_rewritten(0));
        assert_eq!(s.overlay_len(), 0);
        drop(s);

        // 只给 id=0 写一条重组结果（三个义项合并成一个）
        StoreWriter::write_overlay(
            &dir,
            2,
            &[(0, vec![Sense { text: "合并后".into(), ..Default::default() }])],
        )
        .unwrap();

        let s = Store::open(&dir).unwrap();
        assert_eq!(s.entry(0).unwrap().senses.len(), 1, "entry() 应当套上旁路表");
        assert_eq!(s.entry(0).unwrap().senses[0].text, "合并后");
        assert!(s.is_rewritten(0));
        assert_eq!(s.overlay_len(), 1);

        // 关键：接地校验要拿原始义项数比对 from 下标。
        // 若这里返回 1 而不是 3，下一轮导入就会把合法的 from=[2] 判成越界，
        // 而且越导入越严重 —— 这条断言就是为了钉死那个 bug。
        assert_eq!(s.entry_raw(0).unwrap().senses.len(), 3, "entry_raw() 必须绕过旁路表");
        assert_eq!(s.entry_raw(0).unwrap().senses[2].text, "甲-原始2");

        // 没写进旁路表的那条完全不受影响
        assert_eq!(s.entry(1).unwrap().senses.len(), 3);
        assert!(!s.is_rewritten(1));

        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_raw_never_loads_the_overlay() {
        let dir = tmp("open-raw");
        tiny_store(&dir).unwrap();
        StoreWriter::write_overlay(
            &dir,
            2,
            &[(0, vec![Sense { text: "合并后".into(), ..Default::default() }])],
        )
        .unwrap();

        let s = Store::open(&dir).unwrap();
        assert_eq!(s.entry(0).unwrap().senses.len(), 1);
        drop(s);

        // 构建期走这条路，看不到旁路表，也就不会自我毒化
        let s = Store::open_raw(&dir).unwrap();
        assert_eq!(s.entry(0).unwrap().senses.len(), 3, "open_raw 不该加载旁路表");
        assert!(!s.is_rewritten(0));
        assert_eq!(s.overlay_len(), 0);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_overlay_is_ignored_rather_than_panicking() {
        let dir = tmp("short-overlay");
        tiny_store(&dir).unwrap();
        // idx 短到放不下任何条目
        std::fs::write(dir.join("llm.bin"), b"garbage").unwrap();
        std::fs::write(dir.join("llm.idx"), [0u8; 8]).unwrap();

        let s = Store::open(&dir).unwrap();
        assert_eq!(s.entry(0).unwrap().senses.len(), 3);
        assert!(!s.is_rewritten(0));
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
