#!/usr/bin/env bash
# build.sh <target> [output] [part...]
# the parts are app, svpflow and rife, each one puts its files into the same package folder
set -euo pipefail

target="${1:?target is required}"
output="${2:-dist}"
parts=("${@:3}")
[[ ${#parts[@]} -gt 0 ]] || parts=(app svpflow rife)
zig_version=0.15.2
zig_package=02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239
zigbuild_version=v0.23.4
zigbuild_package=6b69963040818ca1ef573d8135a95aa902e17cf7f873c32c6426642dadfe7f59
xwin_version=v0.23.1
xwin_package=c492c6dfb7e5ac0eee586b796e0fc950ba13077e7f0bbb046f445d71790d5360
sdk_version=26.1
sdk_package=beee7212d265a6d2867d0236cc069314b38d5fb3486a6515734e76fa210c784c
svpflow_release=nightly-20261010-deadbad3e5ee
precotti_release=nightly-20261008-deadbadb5f7f

case "$target" in
  x86_64-unknown-linux-gnu)
    name=interpolini-linux-x86_64
    svpflow_package=1f7a197fe408cff20968f07f5c7bb3f343479c553dbda37e98fed66abb76630b
    precotti_package=b88870e389747774cc49bf45e79064834f78ab99398838ecabcdd2a2c09ca39d
    exe='' prefix=lib suffix=.so
    ;;
  x86_64-pc-windows-msvc)
    name=interpolini-windows-x86_64
    svpflow_package=376418bbb43fd4489ae0a75205299664fddcb30ff095fe5d16d8eaf94848a2fa
    precotti_package=0b7a93765fd4e53421565b2795436dd20abe638e4805294cb70a98c7f05457d2
    exe=.exe prefix='' suffix=.dll
    ;;
  aarch64-apple-darwin | x86_64-apple-darwin)
    name=interpolini-macos-arm64
    svpflow_package=fc286ce9c3147028d8d691d3736c3dcd867d1399817fa42d155a74e93f1dcc25
    precotti_package=82d18b049b9ffd8ee2a48c5c297f9543fbb11702ab314b66a92c00de3c698457
    if [[ "$target" == x86_64-* ]]; then
      name=interpolini-macos-x86_64
      svpflow_package=4e60bfa867780cc51c9d7039f447964189c2a53ec8ddd69198d6eada6bcd009d
      precotti_package=ceeae2403ed65264c494f9bca9137eea2da8e9abce03286b2eedbaa4d39b4d3e
    fi
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
if [[ "$target" == *-apple-darwin && "$(uname -s)" == Linux ]]; then
  export ZIG="$work/zig-x86_64-linux-$zig_version/zig"
  export ZIG_TARGET="${target%%-*}-macos.$MACOSX_DEPLOYMENT_TARGET"
  export SDKROOT="$work/MacOSX$sdk_version.sdk"
  [[ -x "$ZIG" ]] || unpack "https://ziglang.org/download/$zig_version/zig-x86_64-linux-$zig_version.tar.xz" "$zig_package"
  zigbuild="$work/cargo-zigbuild-x86_64-unknown-linux-gnu"
  [[ -d "$zigbuild" ]] || unpack "https://github.com/rust-cross/cargo-zigbuild/releases/download/$zigbuild_version/cargo-zigbuild-x86_64-unknown-linux-gnu.tar.xz" "$zigbuild_package"
  [[ -d "$SDKROOT" ]] || unpack "https://github.com/joseluisq/macosx-sdks/releases/download/$sdk_version/MacOSX$sdk_version.sdk.tar.xz" "$sdk_package"
  export PATH="${ZIG%/*}:$zigbuild:$PATH"
  # the step that reads the ffmpeg headers for rust needs the headers of the compiler and of the system
  headers="--sysroot=$SDKROOT -isystem $(clang -print-resource-dir)/include -isystem $SDKROOT/usr/include"
  build=(env "BINDGEN_EXTRA_CLANG_ARGS_$target=$headers" cargo zigbuild)
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

# the rife plugins and ffmpeg
precotti() {
  cooked="$work/precotti-$precotti_release-$target"
  export FFMPEG_DIR="$cooked/ffmpeg"
  [[ ! -d "$cooked" ]] || return 0
  curl -fsSL "https://github.com/Z1xus/precotti/releases/download/$precotti_release/$target.tar.gz" -o "$work/download"
  check "$work/download" "$precotti_package"
  mkdir "$cooked"
  # from a pipe: gnu tar takes a path with a drive letter for a host
  tar -xzf - -C "$cooked" < "$work/download"
  rm "$work/download"
}

app() {
  precotti
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
    echo "open-svpflow: $svpflow_release"
    echo "precotti: $precotti_release"
    rustc -Vv
  } > "$bin/BUILD.txt"
}

svpflow() {
  local source="$work/open-svpflow-$svpflow_release-$target"
  if [[ ! -d "$source" ]]; then
    curl -fsSL "https://github.com/Z1xus/open-svpflow/releases/download/$svpflow_release/$target.zip" -o "$work/download"
    check "$work/download" "$svpflow_package"
    unzip -q "$work/download" -d "$source"
    rm "$work/download"
  fi
  cp "$source"/"$prefix"{open_svpflow,svpflow1_vs,svpflow2_vs}"$suffix" "$bin/"
  cp "$source/LICENSE" "$stage/licenses/open-svpflow.txt"
  cp "$source/CRATES.txt" "$stage/licenses/open-svpflow-crates.txt"
}

rife() {
  precotti
  cp -r "$cooked/bin/." "$bin/"
  cp "$cooked/licenses"/* "$stage/licenses/"
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
