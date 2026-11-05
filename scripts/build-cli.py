#!/usr/bin/env python3
"""为统一安装器发布一个指定平台的无凭据程序；不推送到远程。"""
import argparse
import hashlib
from pathlib import Path
import shutil
import subprocess


def main():
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description="生成 Chromie CLI 平台产物与校验值")
    parser.add_argument("--binary", type=Path, default=root / "target/debug/chromie")
    parser.add_argument("--target")
    parser.add_argument("--output", type=Path, default=root / "web/pkg/cli")
    args = parser.parse_args()
    target = args.target
    if not target:
        version = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
        target = next(line.removeprefix("host: ") for line in version.splitlines() if line.startswith("host: "))
    supported = {"aarch64-apple-darwin", "x86_64-apple-darwin", "aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"}
    if target not in supported or not args.binary.is_file():
        parser.error("缺少程序或该 target 暂不支持自动安装")
    destination = args.output / target
    destination.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(args.binary, destination / "chromie")
    digest = hashlib.sha256((destination / "chromie").read_bytes()).hexdigest()
    (destination / "sha256").write_text(digest + "\n")
    print(f"已生成 {target} CLI 与 SHA-256：{destination}")


if __name__ == "__main__":
    main()
