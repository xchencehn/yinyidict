#!/usr/bin/env bash
# 为 windows-gnu 工具链缺失的系统库现造一个导入库。
#
# rustup 的 rust-mingw 只带了 44 个常用系统库的 .a，eframe 依赖链要的
# shlwapi 不在其中。这里从系统 DLL 的导出表反推出 .def，再用 llvm-dlltool
# 生成 .a —— llvm-dlltool 是自包含的，不像 GNU dlltool 那样还要外部汇编器。
#
#   tools/make-import-lib.sh shlwapi [更多库名...]
set -euo pipefail

TB="$(rustc --print sysroot)/lib/rustlib/x86_64-pc-windows-gnu/bin"
OUT="$(cd "$(dirname "$0")/.." && pwd)/.bin/lib"
mkdir -p "$OUT"

for name in "$@"; do
  dll="/c/Windows/System32/$name.dll"
  [ -f "$dll" ] || { echo "找不到 $dll" >&2; exit 1; }
  def="$OUT/$name.def"
  { echo "LIBRARY $name.dll"; echo "EXPORTS";
    "$TB/llvm-readobj.exe" --coff-exports "$dll" \
      | sed -n 's/^  Name: \(.*\)$/\1/p'; } > "$def"
  "$OUT/../llvm-dlltool.exe" -d "$def" -D "$name.dll" -l "$OUT/lib$name.a" -m i386:x86-64
  echo "生成 $OUT/lib$name.a （$(grep -c . "$def") 行 def）"
done
