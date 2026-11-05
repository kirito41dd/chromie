#!/bin/sh
set -eu

chromie_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
chromie_wasm_pack="$chromie_root/.local/tools/bin/wasm-pack"
if [ ! -x "$chromie_wasm_pack" ]; then
    echo '缺少本地 wasm-pack，请按 docs/开发运行.md 安装构建工具。' >&2
    exit 1
fi

# 不自动安装 wasm-opt，保持用户已确认的最小构建工具链。
exec "$chromie_wasm_pack" build "$chromie_root/web" \
    --release --target web --no-pack --no-opt
