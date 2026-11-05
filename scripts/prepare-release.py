#!/usr/bin/env python3
"""校验四个平台的客户端，生成无实例数据的发行材料；不上传或发布。"""
import argparse
import hashlib
from pathlib import Path
import re
import tarfile


ROOT = Path(__file__).resolve().parent.parent
TARGETS = {
    "x86_64-unknown-linux-gnu": ("elf", 62),
    "aarch64-unknown-linux-gnu": ("elf", 183),
    "x86_64-apple-darwin": ("macho", 0x01000007),
    "aarch64-apple-darwin": ("macho", 0x0100000C),
}
LICENSES = ["LICENSE", "THIRD_PARTY_NOTICES.md", "licenses/GPL-3.0-only.txt",
            "web/fonts/OFL.txt", "web/fonts/README.md"]


def version(tag=None):
    package = (ROOT / "Cargo.toml").read_text().split("[workspace]", 1)[0]
    match = re.search(r'^version = "([^"]+)"$', package, re.M)
    if not match or not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", match[1]):
        raise ValueError("当前发行只接受 Cargo 中的稳定版本号")
    if tag is not None and tag != "v" + match[1]:
        raise ValueError("版本标签必须为 v 加 Cargo.toml 中的版本号")
    return match[1]


def digest(path):
    sha = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            sha.update(chunk)
    return sha.hexdigest()


def clients(directory):
    """不信任 artifact 的目录名：同时检查校验值与可执行文件的架构头。"""
    if directory.is_symlink():
        raise ValueError("客户端目录不能是符号链接")
    result = {}
    for target, (kind, machine) in TARGETS.items():
        folder = directory / target
        binary, checksum = folder / "chromie", folder / "sha256"
        if folder.is_symlink() or any(p.is_symlink() or not p.is_file() for p in [binary, checksum]):
            raise ValueError("需要四个平台的完整客户端与校验文件，不能使用符号链接")
        expected = checksum.read_text().strip()
        if not re.fullmatch("[0-9a-f]{64}", expected) or digest(binary) != expected:
            raise ValueError("客户端校验值不符，停止发行")
        with binary.open("rb") as source:
            header = source.read(32)
        valid = (len(header) >= 20 and header[:6] == b"\x7fELF\x02\x01" and
                 int.from_bytes(header[18:20], "little") == machine) if kind == "elf" else (
                     len(header) >= 8 and header[:4] == b"\xcf\xfa\xed\xfe" and
                     int.from_bytes(header[4:8], "little") == machine)
        if not valid:
            raise ValueError("客户端格式或架构与目标目录不符，停止发行")
        result[target] = binary
    return result


def package(output, verified, image):
    if not re.fullmatch(r"(?:docker\.io/)?[a-z0-9]+(?:[._-][a-z0-9]+)*/chromie:[0-9]+\.[0-9]+\.[0-9]+", image):
        raise ValueError("发行镜像需要明确的命名空间与稳定版本标签")
    current = version()
    if image.rsplit(":", 1)[1] != current:
        raise ValueError("镜像版本必须与 Cargo.toml 一致")
    compose = (ROOT / "compose.yaml").read_text()
    replacements = {"  build: .\n": "", "  pull_policy: never\n": "",
                    "${CHROMIE_IMAGE:-chromie:local}": image}
    for old, new in replacements.items():
        if compose.count(old) != 1:
            raise ValueError("源码 Compose 结构已变化，需核对发行转换")
        compose = compose.replace(old, new)
    if output.is_symlink() or (output.exists() and any(output.iterdir())):
        raise ValueError("发行输出目录必须为空，不能覆盖已有资料")
    output.mkdir(parents=True, exist_ok=True)
    (output / "compose.yaml").write_text(compose)
    (output / "使用说明.txt").write_text(
        f"Chromie {current}，镜像 {image}。\n"
        "在独立目录保存 compose.yaml，以下命令在 Bash 与 Nushell 中相同：\n\n"
        "docker compose pull\ndocker compose up -d\n"
        "docker compose exec chromie chromie-server initial-login\n\n"
        "浏览器访问 http://127.0.0.1:8732/spaces/personal/dashboard。\n"
        "初始密码仅在自己的终端查看，不发送给 Agent。\n"
        "持久保存并分别备份 config 与 data，尤其是 config/instance-key.pem。\n"
        "需要访问服务器时，按源码 README 设置端口和 public_origin。\n"
        "源码、许可与完整使用说明：https://github.com/kirito41dd/chromie\n")
    for target, binary in verified.items():
        path = output / f"chromie-{current}-{target}.tar.gz"
        with tarfile.open(path, "w:gz") as archive:
            for source, name in [(binary, "chromie"), (binary.parent / "sha256", "sha256"),
                                 *((ROOT / name, name) for name in LICENSES)]:
                info = archive.gettarinfo(str(source), arcname=name)
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                info.mtime = 0
                info.mode = 0o755 if name == "chromie" else 0o644
                with source.open("rb") as content:
                    archive.addfile(info, content)
    sums = [f"{digest(p)}  {p.name}\n" for p in sorted(output.iterdir())]
    (output / "SHA256SUMS").write_text("".join(sums))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--clients", type=Path, default=ROOT / "packaging/clients")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--image")
    parser.add_argument("--tag")
    parser.add_argument("--print-version", action="store_true")
    args = parser.parse_args()
    try:
        current = version(args.tag)
        if args.print_version:
            print(current)
            return
        verified = clients(args.clients)
        if args.output:
            package(args.output, verified, args.image or "docker.io/kirito41dd/chromie:" + current)
        print("四个平台客户端已校验" + ("，发行材料已生成。" if args.output else "。"))
    except (ValueError, OSError, UnicodeError):
        parser.exit(1, "发行材料无效：请检查版本标签、四平台客户端及校验文件，输出目录须为空。\n")


if __name__ == "__main__":
    main()
