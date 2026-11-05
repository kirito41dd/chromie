#!/usr/bin/env python3
"""生成可独立分发的 Compose 与镜像压缩包，不推送到镜像仓库。"""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess


def main():
    root = Path(__file__).resolve().parent.parent
    version = re.search(r'^version = "([^"]+)"', (root / "Cargo.toml").read_text(), re.MULTILINE).group(1)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default=f"chromie:{version}")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--skip-build", action="store_true")
    args = parser.parse_args()
    if not args.skip_build:
        subprocess.run(["docker", "build", "--build-arg", f"CHROMIE_VERSION={version}", "-t", args.image, str(root)], check=True)
    metadata = json.loads(subprocess.check_output(["docker", "image", "inspect", args.image]))[0]
    target = f"{metadata['Os']}-{metadata['Architecture']}"
    output = args.output or root / ".local/docker-release" / target
    output.mkdir(parents=True, exist_ok=True)
    compose = (root / "compose.yaml").read_text().replace("  build: .\n", "").replace("  pull_policy: never\n", "")
    compose = compose.replace("${CHROMIE_IMAGE:-chromie:local}", "${CHROMIE_IMAGE:-" + args.image + "}")
    (output / "compose.yaml").write_text(compose)
    archive = output / f"chromie-{version}-{target}.tar.gz"
    process = subprocess.Popen(["docker", "image", "save", args.image], stdout=subprocess.PIPE)
    try:
        with gzip.open(archive, "wb", compresslevel=6) as destination:
            shutil.copyfileobj(process.stdout, destination, length=1024 * 1024)
        if process.wait() != 0:
            raise RuntimeError("导出 Docker 镜像失败")
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait()
    hasher = hashlib.sha256()
    with archive.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            hasher.update(chunk)
    digest = hasher.hexdigest()
    (output / "sha256.txt").write_text(f"{digest}  {archive.name}\n")
    (output / "使用说明.txt").write_text(
        f"Chromie {version}，镜像平台 {target}。\n"
        "解压后在此目录运行；以下命令在 Bash 与 Nushell 中相同：\n\n"
        f"docker load -i {archive.name}\n"
        "docker compose up -d\n"
        "docker compose exec chromie chromie-server initial-login\n\n"
        "浏览器访问 http://127.0.0.1:8732/spaces/personal/dashboard\n"
        "仅自动创建 config/ 与 data/ 两个持久目录，无需 .env。\n"
        "初始密码只写 config/initial-space.txt，请在自己的终端查看，不发给 Agent。\n"
        "保留并安全备份原 config 与 data；私钥丢失不能用重新生成来修复。\n"
        "MySQL 与 Qdrant 仍需可从 Docker 获取；离线部署也应预先导入这两个官方镜像。\n"
    )
    print(json.dumps({"image": args.image, "platform": target, "output": str(output), "archive_bytes": archive.stat().st_size}, ensure_ascii=False))


if __name__ == "__main__":
    main()
