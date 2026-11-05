#!/usr/bin/env python3
"""本地 curl 薄封装；原文只在私有文件与独立 HTTP 之间流动。"""
import argparse
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import urllib.parse
import uuid


class LocalError(Exception):
    pass


def private_file(path):
    path = Path(path).absolute()
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise LocalError("输入必须为本人持有、权限为 0600 或更严格的普通私有文件，不接受符号链接")
    if not 1 <= info.st_size <= 60000:
        raise LocalError("输入文件须为 1～60,000 字节")
    return path


def connection():
    settings = {}
    if os.environ.get("CHROMIE_CLIENT_CONFIG"):
        path = private_file(os.environ["CHROMIE_CLIENT_CONFIG"])
        if path.stat().st_size > 8192:
            raise LocalError("本机接入配置最大 8 KiB")
        settings = json.loads(path.read_text())
        if not isinstance(settings, dict):
            raise LocalError("本机接入配置须为 JSON 对象")
    base = (settings.get("url", "") if settings else os.environ.get("CHROMIE_URL", "")).rstrip("/")
    space = settings.get("space", "") if settings else os.environ.get("CHROMIE_SPACE", "")
    key = settings.get("mcp_key", "") if settings else os.environ.get("CHROMIE_MCP_KEY", "")
    url = urllib.parse.urlsplit(base)
    if not url.hostname or url.username is not None or url.password is not None or url.query or url.fragment:
        raise LocalError("CHROMIE_URL 不能携带凭据、查询或片段")
    if url.scheme not in ("http", "https"):
        raise LocalError("Secret 服务地址须使用 HTTP 或 HTTPS")
    if len(space) > 32 or not re.fullmatch(r"[a-z]+(?:-[a-z]+)*", space):
        raise LocalError("CHROMIE_SPACE 须为短英文空间标识")
    if not re.fullmatch(r"cm_mcp_[0-9a-f]{64}", key):
        raise LocalError("请在本机配置有效的 MCP 接入密钥，并在 Web 开启该接入的 Secret 存取权限")
    # 配置经 stdin 传给 curl，进程命令行与工具参数都不含接入密钥。
    config = ('header = "Authorization: Bearer ' + key + '"\n').encode("utf-8")
    protocol = "=http,https" if url.scheme == "http" else "=https"
    return base + "/spaces/" + space + "/secrets", space, config, protocol


def reference_id(reference, space):
    ref = urllib.parse.urlsplit(reference)
    if ref.scheme != "secret" or ref.netloc != space or ref.query or ref.fragment:
        raise LocalError("Secret 引用必须属于当前空间，不能跨空间取用")
    return str(uuid.UUID(ref.path.removeprefix("/")))


def curl(config, protocol, url, arguments):
    environment = dict(os.environ)
    for name in ("CHROMIE_PASSWORD", "CHROMIE_MCP_KEY", "CHROMIE_HOOK_KEY", "DATABASE_URL", "MYSQL_PASSWORD", "MYSQL_ROOT_PASSWORD", "CHROMIE_QDRANT_API_KEY"):
        environment.pop(name, None)
    result = subprocess.run([
        "curl", "--disable", "--config", "-", "--silent", "--fail", "--no-location",
        "--proto", protocol, "--proto-redir", "=https", "--max-time", "20", "--max-filesize", "60000",
        *arguments, "--url", url,
    ], input=config, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=environment, check=False)
    if result.returncode:
        raise LocalError("HTTP 请求未成功，curl 状态为 " + str(result.returncode) + "；未输出响应原文")
    return result.stdout


def save(config, protocol, url, target):
    target = Path(target).absolute()
    target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    parent = target.parent.resolve(strict=True)
    info = parent.stat()
    if info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise LocalError("取用目标必须位于本人持有、权限为 0700 的私有目录")
    destination = parent / target.name
    if destination.is_symlink() or (destination.exists() and not destination.is_file()):
        raise LocalError("取用目标不能是符号链接或目录")
    fd, temporary = tempfile.mkstemp(prefix=".chromie-secret-", dir=parent)
    os.close(fd)
    try:
        content_type = curl(config, protocol, url, ["--output", temporary, "--write-out", "%{content_type}"])
        if content_type.split(b";", 1)[0].strip() != b"application/octet-stream":
            raise LocalError("响应不是 Secret 二进制数据，未替换原文件")
        if not 1 <= Path(temporary).stat().st_size <= 60000:
            raise LocalError("Secret 响应大小无效，未替换原文件")
        os.chmod(temporary, 0o600)
        with open(temporary, "rb") as stream:
            os.fsync(stream.fileno())
        os.replace(temporary, destination)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    return {"written": True, "path": str(destination)}


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description="Secret 文件存取；不回显原文")
    sub = parser.add_subparsers(dest="operation", required=True)
    put = sub.add_parser("put", help="从私有文件录入，label 只放允许公开的说明")
    put.add_argument("label")
    put.add_argument("source")
    put.add_argument("--project")
    get = sub.add_parser("get", help="按引用取用，原文直接写入文件")
    get.add_argument("reference")
    get.add_argument("target")
    update = sub.add_parser("update", help="使用当前记忆修订号更新凭据文件")
    update.add_argument("reference")
    update.add_argument("revision", type=int)
    update.add_argument("source")
    for command in (put, update):
        command.add_argument("--request-id", type=uuid.UUID, default=None, help="写操作重试必须复用同一个 UUID")
    args = parser.parse_args()
    request = str(args.request_id or uuid.uuid4()) if args.operation != "get" else None
    try:
        endpoint, space, config, protocol = connection()
        if args.operation == "get":
            result = save(config, protocol, endpoint + "/" + reference_id(args.reference, space), args.target)
        else:
            source = private_file(args.source)
            if args.operation == "put":
                if not args.label.strip() or len(args.label.encode("utf-8")) > 256:
                    raise LocalError("公开说明须为 1～256 字节，不能填写凭据原文")
                metadata = {"label": args.label, "request_id": request}
                if args.project is not None:
                    metadata["project_key"] = args.project
                method = "POST"
            else:
                if args.revision < 1:
                    raise LocalError("请先读取当前记忆修订号")
                endpoint += "/" + reference_id(args.reference, space)
                metadata = {"request_id": request, "expected_revision": args.revision}
                method = "PUT"
            raw = curl(config, protocol, endpoint + "?" + urllib.parse.urlencode(metadata), [
                "--request", method, "--header", "Content-Type: application/octet-stream", "--data-binary", "@" + str(source),
            ])
            received = json.loads(raw)
            memory_id = str(uuid.UUID(received["memory_id"]))
            if not received.get("hidden") and received.get("secret_ref") != "secret://" + space + "/" + memory_id:
                raise LocalError("返回的 Secret 引用不属于本次空间")
            result = {key: received[key] for key in ("memory_id", "secret_ref", "revision", "readable", "hidden", "processing_state", "replayed") if key in received}
            result["request_id"] = request
        print(json.dumps(result, ensure_ascii=False))
    except LocalError as error:
        print(json.dumps({"error": str(error), "request_id": request}, ensure_ascii=False), file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError, TypeError):
        print(json.dumps({"error": "本地文件、配置或响应处理失败，未输出原文", "request_id": request}, ensure_ascii=False), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
