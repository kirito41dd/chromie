#!/bin/sh
set -eu

# 初始化前安静跳过；不把私有配置、消息或凭据回显到会话。
chromie_plugin_root=${PLUGIN_ROOT:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}
chromie_config_file=${CHROMIE_CLIENT_CONFIG:-"$HOME/.config/chromie/codex/client.json"}
if [ "${CHROMIE_CAPTURE:-}" = off ] || [ ! -f "$chromie_config_file" ] || [ ! -x "$chromie_plugin_root/bin/chromie" ]; then
    printf '{}\n'
    exit 0
fi
CHROMIE_QUEUE_DIR=${CHROMIE_QUEUE_DIR:-"$(dirname -- "$chromie_config_file")/queue"}
export CHROMIE_QUEUE_DIR
exec "$chromie_plugin_root/bin/chromie" capture --config "$chromie_config_file"
