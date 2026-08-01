#!/usr/bin/env bash
# Build the ClawPay component and assemble an installable plugin directory:
#   dist/clawpay/{manifest.toml, clawpay.wasm}
# Install with: zeroclaw plugin install ./dist/clawpay/
set -euo pipefail
cd "$(dirname "$0")/.."

rustup target add wasm32-wasip2 >/dev/null
cargo build --release --target wasm32-wasip2

rm -rf dist/clawpay
mkdir -p dist/clawpay
cp manifest.toml dist/clawpay/
cp target/wasm32-wasip2/release/clawpay.wasm dist/clawpay/

echo "Plugin directory ready:"
ls -lh dist/clawpay/
echo
echo "Install with: zeroclaw plugin install ./dist/clawpay/"
