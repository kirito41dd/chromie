#!/bin/sh
# 安装器不读取空间密码或 Agent 密钥；只下载当前平台的公共构建产物。
set -eu
if [ "$#" -ne 1 ]; then
    printf '%s\n' '用法：sh install.sh <Chromie 服务 Origin>' >&2
    exit 1
fi
chromie_origin=${1%/}
case "$chromie_origin" in
    *\?*|*\#*|*@*|*\ *|*\'*|*\"*) printf '%s\n' '服务地址不能携带凭据、查询或片段。' >&2; exit 1 ;;
esac
case "$chromie_origin" in
    http://*|https://*) ;;
    *) printf '%s\n' '服务地址必须使用 HTTP 或 HTTPS。' >&2; exit 1 ;;
esac
case "$(uname -s):$(uname -m)" in
    Darwin:arm64) chromie_target=aarch64-apple-darwin ;;
    Darwin:x86_64) chromie_target=x86_64-apple-darwin ;;
    Linux:aarch64|Linux:arm64) chromie_target=aarch64-unknown-linux-gnu ;;
    Linux:x86_64) chromie_target=x86_64-unknown-linux-gnu ;;
    *) printf '%s\n' '当前系统或架构尚无自动安装支持。' >&2; exit 1 ;;
esac
command -v curl >/dev/null || { printf '%s\n' '请先安装 curl。' >&2; exit 1; }
umask 077
chromie_tmp=$(mktemp -d "${TMPDIR:-/tmp}/chromie-install.XXXXXX")
trap 'rm -rf "$chromie_tmp"' EXIT HUP INT TERM
chromie_asset="$chromie_origin/integrations/cli/$chromie_target"
curl --fail --silent --show-error --connect-timeout 10 --max-time 30 "$chromie_asset/sha256" -o "$chromie_tmp/sha256" || {
    printf '%s\n' "服务尚未提供 $chromie_target 产物，请联系部署者构建。" >&2; exit 1;
}
curl --fail --silent --show-error --connect-timeout 10 --max-time 300 "$chromie_asset/chromie" -o "$chromie_tmp/chromie"
chromie_expected=$(cat "$chromie_tmp/sha256")
if command -v sha256sum >/dev/null; then
    chromie_actual=$(sha256sum "$chromie_tmp/chromie" | cut -d ' ' -f 1)
elif command -v shasum >/dev/null; then
    chromie_actual=$(shasum -a 256 "$chromie_tmp/chromie" | cut -d ' ' -f 1)
else
    printf '%s\n' '需要 sha256sum 或 shasum 校验下载文件，未安装程序。' >&2; exit 1
fi
if [ "${#chromie_expected}" -ne 64 ] || [ "$chromie_expected" != "$chromie_actual" ]; then
    printf '%s\n' 'SHA-256 校验失败，保留原有程序。' >&2; exit 1
fi
chromie_bin_dir="$HOME/.local/bin"
mkdir -p "$chromie_bin_dir"
if [ -L "$chromie_bin_dir/chromie" ] || [ -d "$chromie_bin_dir/chromie" ]; then
    printf '%s\n' '安装位置是符号链接或目录，请先确认原有安装，未覆盖。' >&2; exit 1
fi
chromie_staging=$(mktemp "$chromie_bin_dir/.chromie.XXXXXX")
trap 'rm -rf "$chromie_tmp"; rm -f "$chromie_staging"' EXIT HUP INT TERM
cp "$chromie_tmp/chromie" "$chromie_staging"
chmod 700 "$chromie_staging"
if ! "$chromie_staging" help >/dev/null 2>&1; then
    printf '%s\n' '程序无法在当前系统运行，请检查架构或系统运行库；保留原有程序。' >&2; exit 1
fi
mv -f "$chromie_staging" "$chromie_bin_dir/chromie"
printf '%s\n' "Chromie 已安装：$chromie_bin_dir/chromie" '接下来运行接入页面提供的 init 命令。需要升级时重新运行本安装器。'

cat <<'CHROMIE_PATH_HELP'
可选：加入当前终端的 PATH，之后可直接运行 chromie：
Bash / Zsh:
  export PATH="$HOME/.local/bin:$PATH"
Nushell:
  $env.PATH = ($env.PATH | prepend ($nu.home-dir | path join '.local' 'bin') | uniq)
持久保存与终端配置说明：chromie path --shell bash 或 chromie path --shell nushell。
CHROMIE_PATH_HELP
