#!/usr/bin/env bash
# build.sh <target> [output] [part...]
# the parts are app, svpflow and rife, each one puts its files into the same package folder
set -euo pipefail

target="${1:?target is required}"
output="${2:-dist}"
parts=("${@:3}")
[[ ${#parts[@]} -gt 0 ]] || parts=(app svpflow rife)
ffmpeg_version=8.1.3
ffmpeg_source=7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3
ffmpeg_release=autobuild-2026-10-06-13-06
ffmpeg_build=ffmpeg-n$ffmpeg_version-14-g330caae0c1
x264_commit=b35605ace3ddf7c1a5d67a2eb553f034aef41d55
moltenvk_version=v1.4.2
moltenvk_package=f95765a6229cb7b915990a2890ce12ebe36a730b021545d3d52ae69ce4c4024e
zig_version=0.15.2
zig_package=02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239
zigbuild_version=v0.23.4
zigbuild_package=6b69963040818ca1ef573d8135a95aa902e17cf7f873c32c6426642dadfe7f59
xwin_version=v0.23.1
xwin_package=c492c6dfb7e5ac0eee586b796e0fc950ba13077e7f0bbb046f445d71790d5360
sdk_version=26.1
sdk_package=beee7212d265a6d2867d0236cc069314b38d5fb3486a6515734e76fa210c784c
svpflow_commit=badc0de4103c9e6e554548004a1aa88e11fa2866

case "$target" in
  x86_64-unknown-linux-gnu)
    name=interpolini-linux-x86_64
    ffmpeg="$ffmpeg_build-linux64-gpl-shared-8.1"
    archive="$ffmpeg.tar.xz"
    hash=a6d0ea7dfef6ef85d86b8acf1c0a2d5288a05bac42c2cb16914e26830da3d344
    exe='' prefix=lib suffix=.so
    ;;
  x86_64-pc-windows-msvc)
    name=interpolini-windows-x86_64
    ffmpeg="$ffmpeg_build-win64-gpl-shared-8.1"
    archive="$ffmpeg.zip"
    hash=751c56e0b63426426487ab4048031b0166281c59c0a7e33ef7dd7428495e1d8a
    exe=.exe prefix='' suffix=.dll
    ;;
  aarch64-apple-darwin | x86_64-apple-darwin)
    name=interpolini-macos-arm64
    [[ "$target" == x86_64-* ]] && name=interpolini-macos-x86_64
    ffmpeg="ffmpeg-$ffmpeg_version-$target"
    exe='' prefix=lib suffix=.dylib
    export MACOSX_DEPLOYMENT_TARGET=12.0
    ;;
  *)
    echo 'unsupported target' >&2
    exit 1
    ;;
esac

root="$(pwd -P)"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
work="$root/target/ci"
if command -v cygpath >/dev/null 2>&1; then
  root="$(cygpath -m "$root")"
  cargo_home="$(cygpath -m "$cargo_home")"
  work="$(cygpath -m "$work")"
fi
stage="$output/$name"
bin="$stage"
# on macos the app is a bundle, and all of its files are next to the program in it
[[ "$target" == *-apple-darwin ]] && bin="$stage/interpolini.app/Contents/MacOS"
mkdir -p "$work" "$bin" "$stage/licenses"
git() { command git -c safe.directory="$root" "$@"; }
sha() { sha256sum "$@" 2>/dev/null || shasum -a 256 "$@"; }
# stops when a download is not the file that is pinned
# an old bash, like the one of macos, does not stop for a test in [[ ]] alone
check() {
  if [[ "$(sha "$1" | cut -d' ' -f1)" != "$2" ]]; then
    echo "$1 does not have the pinned hash" >&2
    exit 1
  fi
}
# the source of a project at one commit
source_at() {
  [[ ! -d "$3" ]] || return 0
  command git init -q "$3"
  command git -C "$3" fetch -q --depth 1 "$1" "$2"
  command git -C "$3" checkout -q FETCH_HEAD
}

flags=("--remap-path-prefix=$root=/src" "--remap-path-prefix=$cargo_home=/cargo")
case "$target" in
  x86_64-pc-windows-msvc)
    if [[ "$(uname -s)" != Linux ]]; then
      msvc_bin="$(cygpath -u "${VCToolsInstallDir:?Windows build tools are not initialized}")/bin/Hostx64/x64"
      test -f "$msvc_bin/link.exe"
      export PATH="$msvc_bin:$PATH"
      CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER="$(cygpath -m "$msvc_bin/link.exe")"
      export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER
    fi
    # the c runtime is inside the programs, so windows needs no extra package for it
    flags+=(-C target-cpu=x86-64-v3 -C target-feature=+crt-static -C link-arg=/Brepro)
    ;;
  x86_64-unknown-linux-gnu)
    flags+=(-C target-cpu=x86-64-v3 -C 'link-arg=-Wl,--disable-new-dtags,-rpath,$ORIGIN')
    ;;
  aarch64-apple-darwin)
    flags+=(-C 'link-arg=-Wl,-rpath,@loader_path')
    ;;
  x86_64-apple-darwin)
    flags+=(-C target-cpu=x86-64-v3 -C 'link-arg=-Wl,-rpath,@loader_path')
    ;;
esac

# gets an archive, checks it against its pinned hash and unpacks it into the work folder
unpack() {
  curl -fsSL "$1" -o "$work/download"
  check "$work/download" "$2"
  tar -xf "$work/download" -C "$work"
  rm "$work/download"
}

# on linux, macos is built with zig as the compiler and the linker, and with the sdk of apple
build=(cargo build)
cross=(./configure)
cmake_flags=()
jobs="$(getconf _NPROCESSORS_ONLN)"
if [[ "$target" == *-apple-darwin && "$(uname -s)" == Linux ]]; then
  export ZIG="$work/zig-x86_64-linux-$zig_version/zig"
  export ZIG_TARGET="${target%%-*}-macos.$MACOSX_DEPLOYMENT_TARGET"
  export SDKROOT="$work/MacOSX$sdk_version.sdk"
  [[ -x "$ZIG" ]] || unpack "https://ziglang.org/download/$zig_version/zig-x86_64-linux-$zig_version.tar.xz" "$zig_package"
  zigbuild="$work/cargo-zigbuild-x86_64-unknown-linux-gnu"
  [[ -d "$zigbuild" ]] || unpack "https://github.com/rust-cross/cargo-zigbuild/releases/download/$zigbuild_version/cargo-zigbuild-x86_64-unknown-linux-gnu.tar.xz" "$zigbuild_package"
  [[ -d "$SDKROOT" ]] || unpack "https://github.com/joseluisq/macosx-sdks/releases/download/$sdk_version/MacOSX$sdk_version.sdk.tar.xz" "$sdk_package"
  export PATH="${ZIG%/*}:$zigbuild:$PATH"
  tools="$work/zig-$target"
  mkdir -p "$tools"
  cp .github/zig-cc "$tools/cc"
  cp .github/zig-cc "$tools/c++"
  for tool in ar ranlib; do
    printf '#!/bin/sh\nexec "%s" %s "$@"\n' "$ZIG" "$tool" > "$tools/$tool"
    chmod +x "$tools/$tool"
  done
  processor="${target%%-*}"
  {
    echo "set(CMAKE_SYSTEM_NAME Darwin)"
    echo "set(CMAKE_SYSTEM_PROCESSOR ${processor/aarch64/arm64})"
    echo "set(CMAKE_C_COMPILER $tools/cc)"
    echo "set(CMAKE_CXX_COMPILER $tools/c++)"
    echo "set(CMAKE_AR $tools/ar)"
    echo "set(CMAKE_RANLIB $tools/ranlib)"
    echo "set(CMAKE_OSX_SYSROOT $SDKROOT)"
  } > "$tools/toolchain.cmake"
  # the step that reads the ffmpeg headers for rust needs the headers of the compiler and of the system
  headers="--sysroot=$SDKROOT -isystem $(clang -print-resource-dir)/include -isystem $SDKROOT/usr/include"
  build=(env "BINDGEN_EXTRA_CLANG_ARGS_$target=$headers" cargo zigbuild)
  cmake_flags=("-DCMAKE_TOOLCHAIN_FILE=$tools/toolchain.cmake" -DCMAKE_INTERPROCEDURAL_OPTIMIZATION=OFF)
  # only for the configure step of x264: a build script of rust must not get these, it runs on linux
  cross=(env "CC=$tools/cc" "AR=$tools/ar" "RANLIB=$tools/ranlib" ./configure --host="${target%%-*}-apple-darwin")
  ffmpeg_cross=(--enable-cross-compile --target-os=darwin --arch="${target%%-*}" "--cc=$tools/cc" "--ar=$tools/ar"
    "--ranlib=$tools/ranlib" --nm=llvm-nm --strip=true)
fi
# on linux, windows is built with clang and the sdk of microsoft, which cargo-xwin gets and sets up
if [[ "$target" == x86_64-pc-windows-msvc && "$(uname -s)" == Linux ]]; then
  xwin="$work/cargo-xwin"
  if [[ ! -x "$xwin/cargo-xwin" ]]; then
    mkdir -p "$xwin"
    curl -fsSL "https://github.com/rust-cross/cargo-xwin/releases/download/$xwin_version/cargo-xwin-$xwin_version.x86_64-unknown-linux-musl.tar.gz" -o "$work/download"
    check "$work/download" "$xwin_package"
    tar -xzf "$work/download" -C "$xwin"
    rm "$work/download"
  fi
  export PATH="$xwin:$PATH" XWIN_CACHE_DIR="$work/xwin"
  rustup target add "$target"
  eval "$(cargo xwin env --target "$target")"
  cmake_flags=("-DCMAKE_TOOLCHAIN_FILE=$XWIN_CACHE_DIR/cmake/clang-cl/$target-toolchain.cmake")
  # the flags of cargo-xwin stay, and one variable for all flags would replace them
  export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="$CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS ${flags[*]}"
else
  printf -v CARGO_ENCODED_RUSTFLAGS '%s\x1f' "${flags[@]}"
  export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS%$'\x1f'}"
fi
unset RUSTFLAGS
export CARGO_INCREMENTAL=0
SOURCE_DATE_EPOCH="$(git show -s --format=%ct HEAD)"
export SOURCE_DATE_EPOCH
export TZ=UTC
export LC_ALL=C

# the license texts of the crates of the project in this folder: each text one time, under the crates that have it
crates() {
  local texts="$work/licenses-$RANDOM" crate folder file text
  mkdir -p "$texts"
  cargo tree --locked --target "$target" -e normal,build --prefix none --format '{p}' \
    | awk '$2 ~ /^v/ && NF == 2 { print $1 "-" substr($2, 2) }' | sort -u | while read -r crate; do
    for folder in "$cargo_home"/registry/src/*/"$crate"; do
      for file in "$folder"/LICEN[SC]E* "$folder"/license* "$folder"/COPYING* "$folder"/NOTICE* "$folder"/UNLICENSE*; do
        [[ -f "$file" ]] || continue
        text="$texts/$(sha "$file" | cut -c1-16)"
        echo "$crate" >> "$text.crates"
        cp "$file" "$text.text"
      done
    done
  done
  for text in "$texts"/*.text; do
    sort -u "${text%.text}.crates" | tr '\n' ' '
    printf '\n\n'
    cat "$text"
    printf '\n\n'
  done > "$1"
  rm -rf "$texts"
}

fetch_ffmpeg() {
  export FFMPEG_DIR="$work/$ffmpeg"
  [[ ! -d "$FFMPEG_DIR" ]] || return 0
  if [[ "$target" == *-apple-darwin ]]; then
    # nobody builds ffmpeg for macos so we do it ourselves, lovely
    # it has the encoders of the system, and x264 inside it as the software encoder
    local x264="$work/x264-$target"
    source_at https://github.com/mirror/x264.git "$x264_commit" "$work/x264-$x264_commit"
    (
      cd "$work/x264-$x264_commit"
      "${cross[@]}" --prefix="$x264" --enable-static --enable-pic --disable-cli
      make -j"$jobs" install
    )
    unpack "https://ffmpeg.org/releases/ffmpeg-$ffmpeg_version.tar.xz" "$ffmpeg_source"
    (
      cd "$work/ffmpeg-$ffmpeg_version"
      PKG_CONFIG_PATH="$x264/lib/pkgconfig" ./configure --prefix="$FFMPEG_DIR" --install-name-dir=@rpath \
        --enable-shared --disable-static --disable-programs --disable-doc --disable-avdevice --disable-avfilter \
        --disable-autodetect --enable-videotoolbox --enable-audiotoolbox --enable-zlib --enable-gpl --enable-libx264 \
        ${ffmpeg_cross[@]+"${ffmpeg_cross[@]}"}
      make -j"$jobs" install
      cp COPYING.GPLv2 "$FFMPEG_DIR/LICENSE.txt"
    )
    return 0
  fi
  curl -fsSL "https://github.com/BtbN/FFmpeg-Builds/releases/download/$ffmpeg_release/$archive" -o "$work/$archive"
  check "$work/$archive" "$hash"
  case "$archive" in
    *.zip) unzip -q "$work/$archive" -d "$work" ;;
    *) tar -xf "$work/$archive" -C "$work" ;;
  esac
}

app() {
  fetch_ffmpeg
  rustup target add "$target"
  "${build[@]}" --release --locked --target "$target"
  cp "target/$target/release/interpolini$exe" "target/$target/release/interpolini-cli$exe" "$bin/"
  cp LICENSE "$stage/"
  if [[ "$target" == x86_64-pc-windows-msvc ]]; then
    cp "$FFMPEG_DIR/bin"/{avcodec,avformat,avutil,swscale,swresample}-*.dll "$bin/"
  else
    # the libraries with one number in their name, which is the name that the app asks for
    for library in "$FFMPEG_DIR"/lib/lib*; do
      if [[ "${library##*/}" =~ ^lib(avcodec|avformat|avutil|swscale|swresample)(\.so\.[0-9]+|\.[0-9]+\.dylib)$ ]]; then
        cp -L "$library" "$bin/"
      fi
    done
  fi
  if [[ "$target" == *-apple-darwin ]]; then
    mkdir -p "$stage/interpolini.app/Contents/Resources"
    cp assets/interpolini.icns "$stage/interpolini.app/Contents/Resources/"
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
    sed "s/VERSION/$version/" .github/Info.plist > "$stage/interpolini.app/Contents/Info.plist"
  fi
  # the art of the themes is on the assets branch: ci checks it out into a folder, a local build reads the branch
  if [[ -d target/art/themes ]]; then
    cp -r target/art/themes "$bin/"
  else
    git archive refs/heads/assets themes | tar -x -C "$bin"
  fi
  crates "$stage/licenses/crates.txt"
  cp "$FFMPEG_DIR/LICENSE.txt" "$stage/licenses/ffmpeg.txt"
  {
    echo "Commit: $(git rev-parse HEAD)"
    echo "Target: $target"
    echo "FFmpeg: $ffmpeg"
    echo "open-svpflow: $svpflow_commit"
    rustc -Vv
  } > "$bin/BUILD.txt"
}

svpflow() {
  local source="$work/open-svpflow-$svpflow_commit"
  source_at https://github.com/Z1xus/open-svpflow.git "$svpflow_commit" "$source"
  if [[ "$target" == x86_64-pc-windows-msvc && "$(uname -s)" == Linux ]]; then
    # yes, this copies a file to a name with a backslash in it. i am not very proud of that
    for crate in svpflow1 svpflow2; do
      cp "$source/crates/$crate/${crate}_vs.def" "$source/crates/$crate\\${crate}_vs.def"
    done
  fi
  (
    cd "$source"
    rustup target add "$target"
    "${build[@]}" --release --locked --target "$target" --target-dir "$work/svpflow" \
      -p svpflow1 -p svpflow2 -p svpflow-capi
    crates "$work/svpflow-crates.txt"
  )
  cp "$work/svpflow/$target/release"/"$prefix"{open_svpflow,svpflow1_vs,svpflow2_vs}"$suffix" "$bin/"
  cp "$source/LICENSE" "$stage/licenses/open-svpflow.txt"
  cp "$work/svpflow-crates.txt" "$stage/licenses/open-svpflow-crates.txt"
}

rife() {
  local built="$work/rife-$target"
  cmake -S rife -B "$built" -G Ninja ${cmake_flags[@]+"${cmake_flags[@]}"}
  cmake --build "$built"
  mkdir -p "$bin/rife"
  cp -r "$built/models/rife-v4.6" "$bin/rife/"
  cp "$built/${prefix}interpolini_rife$suffix" "$bin/"
  cp "$built/source/LICENSE" "$stage/licenses/rife-ncnn-vulkan.txt"
  cp "$built/_deps/ncnn-src/LICENSE.txt" "$stage/licenses/ncnn.txt"
  if [[ "$target" == *-apple-darwin ]]; then
    # apple killed vulkan so we bring our own
    [[ -d "$work/MoltenVK" ]] || unpack "https://github.com/KhronosGroup/MoltenVK/releases/download/$moltenvk_version/MoltenVK-macos.tar" "$moltenvk_package"
    cp "$work/MoltenVK/MoltenVK/dynamic/dylib/macOS/libMoltenVK.dylib" "$bin/"
    cp "$work/MoltenVK/LICENSE" "$stage/licenses/moltenvk.txt"
  else
    # macos has no tensorrt
    cp "$built/${prefix}interpolini_rife_trt$suffix" "$bin/"
    cp "$built/_deps/tensorrt-src/LICENSE" "$stage/licenses/tensorrt-headers.txt"
  fi
}

for part in "${parts[@]}"; do
  case "$part" in
    app | svpflow | rife) "$part" ;;
    *)
      echo "unknown part $part" >&2
      exit 1
      ;;
  esac
done
