#!/usr/bin/env python3
"""两平台验收后发布索引；固定版本不覆盖，重跑旧版本不回退 latest。"""
import argparse
import json
from pathlib import Path
import re
import subprocess


VERSION = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"


def inspect(image):
    result = subprocess.run(["docker", "buildx", "imagetools", "inspect", image, "--raw"],
                            capture_output=True, text=True)
    if result.returncode:
        # 只接受目标引用明确不存在；DNS、登录或凭据帮助程序的错误不能当作空仓库。
        if result.stderr.strip().endswith(image + ": not found"):
            return None
        raise ValueError("无法读取已有镜像状态，未修改稳定标签")
    try:
        manifest = json.loads(result.stdout)
        if not isinstance(manifest, dict):
            raise ValueError("镜像索引格式无效")
        return manifest
    except json.JSONDecodeError as error:
        raise ValueError("镜像索引格式无效") from error


def children(manifest):
    # 发行索引必须只含两个经过验收的 Linux 平台；不以标签名字猜测架构。
    items = manifest.get("manifests", [])
    if not isinstance(items, list) or len(items) != 2:
        raise ValueError("已有固定版本不是预期的两平台索引")
    result = {}
    for item in items:
        if not isinstance(item, dict) or not isinstance(item.get("platform"), dict):
            raise ValueError("镜像平台描述无效")
        platform = item["platform"]
        arch, digest = platform.get("architecture"), item.get("digest")
        if platform.get("os") != "linux" or arch not in {"amd64", "arm64"} or arch in result:
            raise ValueError("镜像平台不匹配")
        result[arch] = digest
    return result


def publish(image, version, revision, digests):
    if not re.fullmatch(r"docker\.io/[a-z0-9]+(?:[._-][a-z0-9]+)*/chromie", image):
        raise ValueError("镜像命名空间无效")
    if not re.fullmatch(VERSION, version) or not re.fullmatch("[0-9a-f]{40}", revision):
        raise ValueError("发行版本或源码修订无效")
    if set(digests) != {"amd64", "arm64"} or any(not re.fullmatch(r"sha256:[0-9a-f]{64}", v) for v in digests.values()):
        raise ValueError("两平台摘要缺失或无效")
    if len(set(digests.values())) != 2:
        raise ValueError("两平台不能指向同一个单平台镜像摘要")
    pinned, latest = inspect(image + ":" + version), inspect(image + ":latest")
    if pinned is not None and children(pinned) != digests:
        raise ValueError("此版本已存在且内容不同，不能覆盖；请发布新版本")
    update_latest = True
    if latest is not None:
        annotations = latest.get("annotations", {})
        previous = annotations.get("org.opencontainers.image.version", "") if isinstance(annotations, dict) else ""
        if not isinstance(previous, str) or not re.fullmatch(VERSION, previous):
            raise ValueError("无法确认 latest 版本，未修改稳定标签")
        if previous == version and children(latest) != digests:
            raise ValueError("latest 的同版本内容不同，不能覆盖")
        update_latest = tuple(map(int, previous.split("."))) <= tuple(map(int, version.split(".")))

    def create(tag, refs):
        subprocess.run(["docker", "buildx", "imagetools", "create", "--tag", tag,
                        "--annotation", "index:org.opencontainers.image.version=" + version,
                        "--annotation", "index:org.opencontainers.image.revision=" + revision, *refs], check=True)

    if pinned is None:
        create(image + ":" + version, [image + "@" + digests[a] for a in ["amd64", "arm64"]])
    if update_latest:
        create(image + ":latest", [image + ":" + version])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--digests", type=Path, required=True)
    args = parser.parse_args()
    try:
        digests = {a: (args.digests / (a + ".txt")).read_text().strip() for a in ["amd64", "arm64"]}
        publish(args.image, args.version, args.revision, digests)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        if isinstance(error, ValueError):
            parser.exit(1, str(error) + "\n")
        parser.exit(1, "镜像发布失败，请核对仓库状态与构建结果；未输出凭据。\n")


if __name__ == "__main__":
    main()
