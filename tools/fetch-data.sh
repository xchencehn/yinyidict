#!/usr/bin/env bash
# 下载并解开全部词典数据源和语音模型。跑一次，约 1.2 GB。
#
#   tools/fetch-data.sh
#
# 之后用 `cargo run --release -p dict-build` 生成 data/index。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RAW="$ROOT/data/raw"
mkdir -p "$RAW" "$ROOT/models" "$ROOT/vendor"

get() { # get <目标文件> <URL>
  [ -s "$1" ] && { echo "已有 $(basename "$1")"; return; }
  echo "下载 $(basename "$1") …"
  curl -sSL --retry 3 -o "$1" "$2"
}

cd "$RAW"
# 中文侧
get cedict.txt.gz    "https://www.mdbg.net/chinese/export/cedict/cedict_1_0_ts_utf-8_mdbg.txt.gz"
# 中文词频 + 词性 + 分词词表。
# 原设计用 SUBTLEX-CH，但它在 crr.ugent.be 的下载已失效（返回 HTML）；
# jieba 的计数同样是大规模语料统计量，覆盖 34.9 万词，运行期只比百分位，不影响打分。
get jieba_dict.txt   "https://raw.githubusercontent.com/fxsjy/jieba/master/jieba/dict.txt"
get Unihan.zip       "https://www.unicode.org/Public/UCD/latest/ucd/Unihan.zip"
# 英文侧
get ecdict.zip       "https://github.com/skywind3000/ECDICT/releases/download/1.0.28/ecdict-sqlite-28.zip"
# 例句
get cmn_sent.tsv.bz2 "https://downloads.tatoeba.org/exports/per_language/cmn/cmn_sentences.tsv.bz2"
get eng_sent.tsv.bz2 "https://downloads.tatoeba.org/exports/per_language/eng/eng_sentences.tsv.bz2"
get links.tar.bz2    "https://downloads.tatoeba.org/exports/links.tar.bz2"

echo "解包 …"
[ -f cedict.txt ]        || gzip -dkf cedict.txt.gz
[ -f cmn_sent.tsv ]      || bunzip2 -kf cmn_sent.tsv.bz2
[ -f eng_sent.tsv ]      || bunzip2 -kf eng_sent.tsv.bz2
[ -f links.csv ]         || tar xjf links.tar.bz2
[ -f ecdict/stardict.db ] || unzip -q -o ecdict.zip -d ecdict/
[ -f unihan/Unihan_Variants.txt ] || unzip -q -o Unihan.zip -d unihan/ Unihan_Readings.txt Unihan_Variants.txt

# sherpa-onnx 运行库（预编译，dlopen 加载，不参与链接）
cd "$ROOT/vendor"
if [ ! -f sherpa-onnx-v1.13.7-win-x64-shared-MT-Release/lib/sherpa-onnx-c-api.dll ]; then
  echo "下载 sherpa-onnx 运行库 …"
  curl -sSL --retry 3 -o s.tar.bz2 \
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.7/sherpa-onnx-v1.13.7-win-x64-shared-MT-Release.tar.bz2"
  tar xjf s.tar.bz2 && rm -f s.tar.bz2
fi

# 语音模型。就这一个 —— 中英同模型同音色，选型已经定下来了。
cd "$ROOT/models"
for m in kokoro-multi-lang-v1_1; do
  [ -d "$m" ] && { echo "已有模型 $m"; continue; }
  echo "下载模型 $m …"
  curl -sSL --retry 3 -o "$m.tar.bz2" \
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/$m.tar.bz2"
  tar xjf "$m.tar.bz2" && rm -f "$m.tar.bz2"
done

echo
echo "完成。接着跑： cargo run --release -p dict-build"
