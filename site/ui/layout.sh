#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
SLINT_EMIT_DEBUG_INFO=1 cargo run --quiet --example regions > ../src/lib/layout.json
