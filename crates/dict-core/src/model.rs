//! 词条数据模型。构建期写入、运行期按需从 mmap 反序列化。

use serde::{Deserialize, Serialize};

pub const KIND_ZH: u8 = 0;
pub const KIND_EN: u8 = 1;

/// 一个义项。`text` 是主释义，`note` 是补充说明，`reg` 是语域/用法标注。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Sense {
    pub pos: String,
    pub text: String,
    pub note: String,
    pub reg: String,
}

/// 一条例句。`a` 是词条本身语言的句子，`b` 是对照译文。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Example {
    pub a: String,
    pub b: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub kind: u8,
    /// 中文侧为简体词头，英文侧为原拼写。
    pub word: String,
    /// 中文侧的繁体形；与简体相同时为空。
    pub trad: String,
    /// 中文侧为带调拼音（空格分隔），英文侧为 IPA。
    pub reading: String,
    /// 中文侧每个音节的声调 0..4（0 = 轻声/无调）。
    pub tones: Vec<u8>,
    /// 展示用的原始词频量：中文侧是语料计数，英文侧是 BNC/COCA 排名。
    pub freq: f32,
    /// 本语料内百分位 0..1。跨语料打分只用这个值。
    pub pct: f32,
    pub pos: String,
    /// 英文侧的形态行（时态/复数/比较级），来自 ECDICT exchange。
    pub forms: String,
    /// 英文侧的考试标签（cet4/gre/…）+ 柯林斯星级。
    pub tags: String,
    pub senses: Vec<Sense>,
    pub examples: Vec<Example>,
    /// 原始 CC-CEDICT / ECDICT 释义，词条页折叠区块用来对照查证。
    pub raw: String,
    pub xrefs: Vec<String>,
}

impl Entry {
    pub fn is_zh(&self) -> bool {
        self.kind == KIND_ZH
    }
}
