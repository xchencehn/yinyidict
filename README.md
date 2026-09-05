# 本地中英词典

离线中英词典桌面应用。原生 Rust 单二进制，不依赖 node / python / java，
断网可用，零遥测。

- **142 万条词头**（中文 12.5 万 + 英文 130 万），逐字符响应 **0.15–2.3 ms**，不做 debounce
- **本机神经 TTS**（sherpa-onnx + Kokoro），GPU 上 RTF ≈ 0.04
- **1 万条高频中文词的义项被 LLM 重组过**，且每一条都有机器可校验的接地凭据

> 目前只支持 Windows。界面和文档是中文的。

## 三个可能对别人有用的点

工程里有几处解法不太显然，都写在 [`CLAUDE.md`](CLAUDE.md) 里了：

**载体句合成再裁剪。** 神经 TTS 喂单个词，韵律模型在无上下文短文本上会退化成
平调 —— 换多好的模型都一样。解法是合成「这个词读作，X」再按最后一处停顿把
目标段切出来。见「发音」一节。

**带接地约束的 LLM 重写。** 让模型重组 CC-CEDICT 的义项，但每个输出义项必须用
`from` 列出它来自哪几条输入义项；导入时逐条比对下标，越界或凭空多出来的整条
打回、回退原文。10,669 条送出、10,645 条通过并写入，源义项覆盖 99.96%。
见「释义质量」一节 —— 包括那个犯了两次的自我毒化 bug。

**七通道融合检索。** 中文词头 / 英文词头 / 拼音 / 首字母 / 子串 / 双向释义
同时跑再归并，外加短前缀预算表（`a` 从 24.5 ms 压到 0.81 ms）。见「检索」一节。

## 下载

[Releases](https://github.com/xchencehn/yinyidict/releases) 里有打好的包（4 MB）。
解开、双击 `setup.bat` 等它把数据下好建好索引（约 850 MB，几分钟），
之后双击 `dict.exe`。按 `Alt+1` 随时唤出。

包里只有程序、没有词库：那些数据不属于本项目，各有各的授权（见下），
不该由我们再分发，所以由程序自己从各家官方地址取。

## 从源码跑

有 MSVC build tools 的机器（大多数 Windows 开发机）什么都不用配：

```bash
tools/fetch-data.sh                    # 一次性，约 1.2 GB 数据源和模型
cargo run --release -p dict-build      # 约 30 秒，产出 data/index
cargo run --release -p dict-app
```

没有 MSVC 的机器先跑一次 `tools/setup-toolchain.sh`，它会装好 windows-gnu 那套
并生成 `.cargo/config.toml`。为什么需要这一步、踩过哪三个坑，见 CLAUDE.md「工具链」。

只想跑测试的话 `cargo test` 就够了，不必下那 1.2 GB —— ETL 的集成测试用的是
`crates/dict-build/tests/fixture` 里一份编出来的微型词库。

## 授权

代码 MIT。

**词库数据不在这个仓库里**，由 `tools/fetch-data.sh` 现拉，各有各的授权：

| 数据 | 来源 | 授权 |
|---|---|---|
| 中文词条 / 拼音 / 释义 | CC-CEDICT | CC BY-SA 3.0 |
| 中文词频 / 词性 | jieba 词典 | MIT |
| 例句 | Tatoeba | CC BY 2.0 FR |
| 异体字 | Unihan | Unicode License |
| 英文全部字段 | ECDICT | **授权不明确，自行判断** |
| 语音模型 | Kokoro v1.1-zh | Apache 2.0 |
| 推理运行库 | sherpa-onnx | Apache 2.0 |

拿它做再分发（尤其是商业用途）之前，这几行需要你自己核实一遍 ——
ShareAlike 的传染性和 ECDICT 的来源问题只在再分发时才触发。
