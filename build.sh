#!/usr/bin/env bash
# Builds the perfmon plugin: plugins/perfmon/target/wasm32-wasip2/release/perfmon.wasm
# Needs Rust (https://rustup.rs). The wasm target is added automatically.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
command -v cargo >/dev/null || { echo "cargo not found: install Rust from https://rustup.rs" >&2; exit 1; }
rustup target list --installed 2>/dev/null | grep -q '^wasm32-wasip2$' || rustup target add wasm32-wasip2
(cd "$HERE" && cargo build --release --target wasm32-wasip2)
ls -la "$HERE/target/wasm32-wasip2/release/perfmon.wasm"
