#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
cargo build --target wasm32-unknown-unknown --release
out=${DOCS_OUT:-../.publish/docs}/ui
mkdir -p "$out"
wasm-bindgen --target web --no-typescript --out-dir "$out" --out-name interpolini_ui target/wasm32-unknown-unknown/release/interpolini_ui.wasm
wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-mutable-globals --enable-reference-types --enable-multivalue "$out/interpolini_ui_bg.wasm" -o "$out/interpolini_ui_bg.wasm"
