#!/usr/bin/env bash
# 准备 windows-gnu 的构建工具链。跑一次即可。
#
#   tools/setup-toolchain.sh
#
# **有 MSVC build tools 的机器不需要跑这个** —— 直接 cargo build 就行。
# 这一步是给没有 MSVC / Windows SDK 的机器用的：那种机器只能走 windows-gnu，
# 而 rustup 自带的那份 mingw 是「仅用于链接」的精简版（见它自己的
# GCC-WARNING.txt），既没有汇编器，GNU ld 链 eframe 这种体量的映像还会产出
# 启动即崩的二进制。详见 .cargo/config.toml.example 里的注释。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# 写进 .cargo/config.toml 的必须是 Windows 形式的路径：cargo 是原生 Windows
# 程序，不认 Git Bash 的 /c/... 写法。
if command -v cygpath >/dev/null 2>&1; then
  WIN_ROOT="$(cygpath -m "$ROOT")"
else
  WIN_ROOT="$(cd "$ROOT" && pwd -W 2>/dev/null || echo "$ROOT")"
fi
BIN="$ROOT/.bin"
mkdir -p "$BIN"

TRIPLE=x86_64-pc-windows-gnu

# rustup override 而不是 rust-toolchain.toml：后者会盖过前者，
# 等于把有 MSVC 的人也强行按到 gnu 上。这条只改本机的 rustup 设置。
if ! rustup toolchain list | grep -q "stable-$TRIPLE"; then
  echo "安装 stable-$TRIPLE …"
  rustup toolchain install "stable-$TRIPLE"
fi
rustup override set "stable-$TRIPLE" >/dev/null
rustup component add llvm-tools --toolchain "stable-$TRIPLE" >/dev/null 2>&1 || true
echo "本目录已固定到 stable-$TRIPLE"

SYSROOT_BIN="$(rustc --print sysroot)/lib/rustlib/$TRIPLE/bin"

# 1) llvm-dlltool：llvm-ar 是 multicall 二进制，以这个名字调用时进 dlltool 模式。
#    raw-dylib（windows-sys / eframe 依赖链要用）需要它，而它自包含，
#    不像 GNU dlltool 那样还要一个外部汇编器。
if [ ! -f "$BIN/llvm-dlltool.exe" ]; then
  [ -f "$SYSROOT_BIN/llvm-ar.exe" ] || {
    echo "缺少 llvm-tools 组件，先跑：rustup component add llvm-tools" >&2
    exit 1
  }
  cp "$SYSROOT_BIN/llvm-ar.exe" "$BIN/llvm-dlltool.exe"
  echo "已放置 llvm-dlltool.exe"
fi

# 2) 完整的 MinGW-w64。取 msvcrt 变体以匹配 Rust 的 windows-gnu 目标
#    （ucrt 变体的 C 运行时和它对不上）。便携解压，不需要管理员权限。
if [ ! -x "$BIN/mingw64/bin/gcc.exe" ]; then
  URL="https://github.com/brechtsanders/winlibs_mingw/releases/download/16.2.0posix-14.0.0-msvcrt-r1/winlibs-x86_64-posix-seh-gcc-16.2.0-mingw-w64msvcrt-14.0.0-r1.zip"
  echo "下载 MinGW-w64（约 263 MB）…"
  curl -sSL --retry 3 -o "$BIN/mingw.zip" "$URL"
  unzip -q -o "$BIN/mingw.zip" -d "$BIN"
  rm -f "$BIN/mingw.zip"
  echo "已解压到 $BIN/mingw64"
fi

# 3) 把 LLD 放进 gcc 找得到的地方。
#    rust-lld 本身就是 LLD 的 multicall 二进制，按 argv[0] 分发 —— 改名成
#    ld.lld 就是 GNU 兼容驱动。放进 x86_64-w64-mingw32/bin/ 是因为那个目录
#    本来就在 gcc 找辅助程序的搜索路径上（gcc -print-search-dirs 可以看到），
#    这样 -fuse-ld=lld 就够了，不需要再写一个 -B 绝对路径。
#    注意不能直接拷 gcc-ld/ld.lld.exe：那个是包装器，它按相对位置去找
#    rust-lld，挪了地方就找不着。
GCC_TB="$BIN/mingw64/x86_64-w64-mingw32/bin"
if [ ! -f "$GCC_TB/ld.lld.exe" ]; then
  cp "$SYSROOT_BIN/rust-lld.exe" "$GCC_TB/ld.lld.exe"
  echo "已放置 ld.lld.exe"
fi

# 4) 生成 .cargo/config.toml。必须是绝对路径：cargo 的 rustflags 不做变量展开，
#    而 rustc 查 dlltool 走的是 PATH，config 里也没法往 PATH 前面插目录。
sed "s|@ROOT@|${WIN_ROOT}|g" "$ROOT/.cargo/config.toml.example" > "$ROOT/.cargo/config.toml"
echo "已生成 .cargo/config.toml"

echo
echo "完成。接着跑： tools/fetch-data.sh && cargo run --release -p dict-build"
