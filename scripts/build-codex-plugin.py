#!/usr/bin/env python3
"""构建无凭据的 Codex 插件与通用采集程序包，不读取本机凭据。"""
import argparse
import json
from pathlib import Path
import subprocess
import zipfile


def main():
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description="构建无凭据的 Codex 插件下载包")
    parser.add_argument("--binary", type=Path, default=root / "target/debug/chromie")
    parser.add_argument("--target", help="指定程序对应的 Rust target，默认本机构建平台")
    parser.add_argument("--output", type=Path, default=root / "web/pkg/chromie-codex.zip")
    parser.add_argument("--client-output", type=Path, default=root / "web/pkg/chromie-client.zip")
    args = parser.parse_args()
    if not args.binary.is_file():
        parser.error("缺少 Chromie 程序，请先 cargo build --bin chromie")
    target = args.target
    if target is None:
        output = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
        target = next(line.removeprefix("host: ") for line in output.splitlines() if line.startswith("host: "))
    if "windows" in target:
        parser.error("首版插件 hook 使用 POSIX shell，请为 macOS 或 Linux 构建")
    source = root / "integrations/codex"
    files = [".agents/plugins/marketplace.json", "README.md", "plugin/.codex-plugin/plugin.json",
             "plugin/skills/setup/SKILL.md", "plugin/hooks/hooks.json", "plugin/scripts/capture.sh", "plugin/assets/icon.svg"]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(args.output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name in files:
            path = source / name
            archive.write(path, "chromie-codex/" + name)
        archive.write(args.binary, "chromie-codex/plugin/bin/chromie")
        archive.write(root / "examples/codex/secret-curl.py", "chromie-codex/plugin/scripts/secret-curl.py")
        archive.writestr("chromie-codex/BUILD.json", json.dumps({"target": target, "version": "0.1.0"}))
        archive.writestr("chromie-codex/plugin/BUILD.json", json.dumps({"target": target, "version": "0.1.0"}))
        for name in ["LICENSE", "THIRD_PARTY_NOTICES.md", "licenses/GPL-3.0-only.txt", "web/fonts/OFL.txt", "web/fonts/README.md"]:
            archive.write(root / name, "chromie-codex/" + name)
    args.client_output.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(args.client_output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        archive.write(args.binary, "chromie-client/bin/chromie")
        for name in ["README.md", "examples/user-message.json", "examples/assistant-message.json"]:
            archive.write(root / "integrations/generic" / name, "chromie-client/" + name)
        archive.writestr("chromie-client/BUILD.json", json.dumps({"target": target, "version": "0.1.0"}))
        for name in ["LICENSE", "THIRD_PARTY_NOTICES.md", "licenses/GPL-3.0-only.txt", "web/fonts/OFL.txt", "web/fonts/README.md"]:
            archive.write(root / name, "chromie-client/" + name)
    print(json.dumps({"plugin": str(args.output), "capture_client": str(args.client_output), "target": target}, ensure_ascii=False))


if __name__ == "__main__":
    main()
