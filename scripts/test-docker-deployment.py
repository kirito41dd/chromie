#!/usr/bin/env python3
"""在独立 Compose 项目与目录验证零配置部署；仅创建虚构资料，不输出凭据。"""
import argparse
import hashlib
from http.cookiejar import CookieJar
import json
import os
from pathlib import Path
import platform
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="chromie:local")
    parser.add_argument("--port", type=int, default=18732)
    parser.add_argument("--compose-file", type=Path)
    parser.add_argument("--client-container", action="store_true", help="仅 Linux：在镜像运行环境执行下载的 CLI，验证容器使用方式")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    test = uuid.uuid4().hex[:10]
    directory = root / ".local" / "docker-tests" / test
    directory.mkdir(parents=True, mode=0o700)
    env = os.environ.copy()
    env.update(CHROMIE_IMAGE=args.image, CHROMIE_PORT=str(args.port))
    compose_file = (args.compose_file or root / "compose.yaml").resolve()
    compose = ["docker", "compose", "--project-directory", str(directory), "-p", f"chromie-check-{test}", "-f", str(compose_file)]
    origin = f"http://127.0.0.1:{args.port}"
    base = origin + "/spaces/personal"
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(CookieJar()))
    results = {}

    def command(argv, data=None, check=True):
        result = subprocess.run(argv, env=env, input=data, capture_output=True)
        with (directory / "commands.log").open("ab") as log:
            log.write(result.stdout + result.stderr)
        if check and result.returncode:
            raise RuntimeError("容器验证命令失败；诊断已保存私有日志，未输出正文")
        return result

    def http(path, method="GET", value=None, headers=None, raw=False):
        request_headers = {"Origin": origin}
        request_headers.update(headers or {})
        # 原生 MCP 客户端（Codex／CLI）不发送浏览器 Origin；Web 接口保留同源校验。
        if path.endswith("/mcp"):
            request_headers.pop("Origin", None)
        data = None
        if value is not None:
            if isinstance(value, bytes):
                data = value
            else:
                data = json.dumps(value).encode()
                request_headers["Content-Type"] = "application/json"
        response = opener.open(urllib.request.Request(origin + path, data=data, method=method, headers=request_headers), timeout=20)
        body = response.read()
        return body if raw else json.loads(body or "{}")

    def login(password):
        return http("/spaces/personal/login", "POST", b"", {"x-chromie-password": password})

    def denied(function, status):
        try:
            function()
        except urllib.error.HTTPError as error:
            assert error.code == status, f"错误状态不符：{error.code}"
            return
        raise AssertionError("应拒绝的请求被接受")

    def rpc(name, arguments):
        data = http("/spaces/personal/mcp", "POST", {"jsonrpc": "2.0", "id": str(uuid.uuid4()), "method": "tools/call", "params": {"name": name, "arguments": arguments}}, {
            "Authorization": "Bearer " + mcp_key,
            "Accept": "application/json, text/event-stream",
            "Mcp-Protocol-Version": "2025-06-18",
        })
        assert "error" not in data and not data["result"].get("isError"), "MCP 操作失败，未输出返回正文"
        return data["result"]["structuredContent"]

    def ready():
        for _ in range(90):
            try:
                http("/ready", raw=True)
                return
            except (urllib.error.URLError, TimeoutError, OSError):
                time.sleep(1)
        raise AssertionError("等待容器就绪超时")

    key = directory / "config/instance-key.pem"
    backup = directory / "original-key.backup"
    try:
        started = time.monotonic()
        command(compose + ["up", "-d", "--no-build", "--pull", "never", "--wait", "--wait-timeout", "180"])
        results["first_start_seconds"] = round(time.monotonic() - started, 1)
        assert {p.name for p in directory.iterdir() if p.is_dir()} == {"config", "data"}
        config = directory / "config"
        password = next(line.removeprefix("密码：") for line in (config / "initial-space.txt").read_text().splitlines() if line.startswith("密码："))
        original_config = {str(p.relative_to(config)): p.read_bytes() for p in config.rglob("*") if p.is_file() and p.name != ".bootstrap.lock"}
        login(password)
        dashboard = http("/spaces/personal/dashboard-data")
        assert dashboard["space_id"] == "personal" and dashboard["memory_count"] == 0
        for path in ["/", "/web/chromie_web.js", "/web/chromie_web_bg.wasm", "/install.sh"]:
            assert len(http(path, raw=True)) > 50
        results["empty_start_login_and_web_assets"] = True
        for path in ["/web/bootstrap.js", "/web/chromie_web.js", "/web/chromie_web_bg.wasm"]:
            with opener.open(urllib.request.Request(origin + path, method="HEAD"), timeout=20) as response:
                etag = response.headers["ETag"]
                assert "must-revalidate" in response.headers["Cache-Control"]
            try:
                opener.open(urllib.request.Request(origin + path, headers={"If-None-Match":etag}), timeout=20)
            except urllib.error.HTTPError as error:
                assert error.code == 304 and error.read() == b""
            else:
                raise AssertionError("公开资源重复请求未返回 304")
        results["public_assets_revalidate_without_body"] = True
        server_help = command(compose + ["exec", "-T", "chromie", "chromie-server", "--help"]).stdout
        assert b"container-serve" in server_help and b"capture" not in server_help
        results["server_command_boundary"] = True
        agent = http("/spaces/personal/agents", "POST", {"agent_name": "虚构容器验收"})["agent"]
        agent_id = agent["agent_id"]
        http(f"/spaces/personal/agents/{agent_id}", "PATCH", {"allow_secrets": True})
        mcp_key = http(f"/spaces/personal/agents/{agent_id}/keys/mcp/reveal", "POST", b"")["key"]
        hook_key = http(f"/spaces/personal/agents/{agent_id}/keys/hook/reveal", "POST", b"")["key"]
        rpc_headers = {"Authorization": "Bearer " + mcp_key, "Accept": "application/json, text/event-stream", "Mcp-Protocol-Version": "2025-06-18"}
        initialized = http("/spaces/personal/mcp", "POST", {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "chromie-docker-check", "version": "0.1.0"}}}, rpc_headers)
        assert initialized["result"]["protocolVersion"] == "2025-06-18"
        http("/spaces/personal/mcp", "POST", {"jsonrpc": "2.0", "method": "notifications/initialized"}, rpc_headers)
        memory = rpc("remember", {"content": "虚构容器验收：时光项目每周五检查备份。", "scope": {"kind": "project", "project_key": "docker-e2e"}, "request_id": str(uuid.uuid4())})
        read = rpc("get_memory", {"memory_id": memory["memory_id"], "view": "current"})
        assert read["revision"] == 1 and "检查备份" in read["content"]
        captured = {"id": str(uuid.uuid4()), "client_type": "generic", "session_id": "docker-e2e", "role": "user", "body": "虚构容器采集验收，不含真实个人资料。", "project_key": "docker-e2e", "occurred_at": "2026-10-30T00:00:00Z"}
        http("/spaces/personal/events", "POST", captured, {"Authorization": "Bearer " + hook_key})
        denied(lambda: http("/spaces/personal/events", "POST", captured, {"Authorization": "Bearer " + mcp_key}), 401)
        results["mcp_and_independent_capture_key"] = True
        secret = uuid.uuid4().hex.encode()
        query = urllib.parse.urlencode({"label": "虚构容器 Secret", "request_id": str(uuid.uuid4()), "project_key": "docker-e2e", "contract": json.dumps({"version": 1, "format": "text", "kind": "password"})})
        receipt = http("/spaces/personal/secrets?" + query, "POST", secret, {"Authorization": "Bearer " + mcp_key, "Content-Type": "application/octet-stream"})
        secret_path = "/spaces/personal/secrets/" + receipt["memory_id"]
        assert http(secret_path, headers={"Authorization": "Bearer " + mcp_key}, raw=True) == secret
        metadata = rpc("get_memory", {"memory_id": receipt["memory_id"], "view": "current"})
        assert secret.decode() not in json.dumps(metadata)
        results["secret_round_trip_and_public_metadata"] = True

        target = {("Darwin", "arm64"): "aarch64-apple-darwin", ("Darwin", "x86_64"): "x86_64-apple-darwin", ("Linux", "aarch64"): "aarch64-unknown-linux-gnu", ("Linux", "x86_64"): "x86_64-unknown-linux-gnu"}.get((platform.system(), platform.machine()))
        if target:
            binary = http(f"/integrations/cli/{target}/chromie", raw=True)
            checksum = http(f"/integrations/cli/{target}/sha256", raw=True).decode().strip()
            assert hashlib.sha256(binary).hexdigest() == checksum
            executable = directory / "downloaded-client"
            executable.write_bytes(binary)
            executable.chmod(0o700)
            def client_command(arguments):
                if args.client_container:
                    assert platform.system() == "Linux", "容器客户端验证仅用于 Linux Docker 主机"
                    return ["docker", "run", "--rm", "--network", "host", "-v", str(directory) + ":/fixture", "--entrypoint", "/fixture/downloaded-client", args.image, *arguments]
                return [str(executable), *arguments]
            assert b"chromie" in command(client_command(["--version"])).stdout.lower()
            client_help = command(client_command(["--help"])).stdout
            assert b"container-serve" not in client_help and b"secret" in client_help
            assert command(client_command(["serve"]), check=False).returncode == 2
            results["client_command_boundary"] = True
            client = directory / "client.json"
            client.write_text(json.dumps({"url": origin, "space": "personal", "agent_id": agent_id, "mcp_key": mcp_key, "hook_key": hook_key}))
            client.chmod(0o600)
            output = directory / "secret-output"
            client_path = "/fixture/client.json" if args.client_container else str(client)
            output_path = "/fixture/secret-output" if args.client_container else str(output)
            command(client_command(["secret", "get", "--config", client_path, "--ref", receipt["secret_ref"], "--revision", "1", "--out", output_path]))
            assert output.read_bytes() == secret
            results["client_download_checksum_and_secret_file"] = {"target": target, "execution": "container" if args.client_container else "host"}
            if not args.client_container:
                import sys
                probe = directory / "exec-probe.py"
                marker = directory / "exec-path"
                probe.write_text("import os,pathlib,sys\np=pathlib.Path(os.environ['CHROMIE_SECRET_FILE'])\nassert p.stat().st_mode & 0o777 == 0o600\nassert p.parent.stat().st_mode & 0o777 == 0o700\npathlib.Path(sys.argv[1]).write_text(str(p))\nprint(p.read_text())\n")
                probe.chmod(0o600)
                execution = command(client_command(["secret", "exec", "--config", client_path, "--ref", receipt["secret_ref"], "--revision", "1", "--purpose", "虚构容器端到端验证", "--", sys.executable, str(probe), str(marker)]))
                outcome = json.loads(execution.stdout)
                assert outcome["outcome"] == "succeeded" and outcome["audit_reported"] and outcome["temporary_files_removed"]
                assert secret not in execution.stdout + execution.stderr
                assert not Path(marker.read_text()).exists()
                results["native_secret_exec_and_cleanup"] = True
            guide = rpc("get_setup_guide", {"topic": "secret"})
            cached = rpc("get_setup_guide", {"topic": "secret", "known_guide_version": guide["guide_version"]})
            assert cached["guide_cached"] and "guide" not in cached and cached["secret_access_enabled"]
            results["versioned_guide_live_status"] = True

        command(compose + ["restart", "chromie"])
        ready()
        assert http("/spaces/personal/dashboard-data")["memory_count"] == 2
        assert rpc("get_memory", {"memory_id": memory["memory_id"], "view": "current"})["revision"] == 1
        assert http(secret_path, headers={"Authorization": "Bearer " + mcp_key}, raw=True) == secret
        results["restart_preserves_cookie_memory_and_secret"] = True
        new_password = uuid.uuid4().hex + uuid.uuid4().hex
        command(compose + ["exec", "-T", "chromie", "chromie-server", "set-password", "personal"], (new_password + "\n").encode())
        command(compose + ["restart", "chromie"])
        ready()
        denied(lambda: login(password), 401)
        login(new_password)
        assert http("/spaces/personal/dashboard-data")["memory_count"] == 2
        results["changed_password_not_reset_at_restart"] = True
        command(compose + ["stop", "chromie"])
        key.rename(backup)
        result = command(compose + ["run", "--rm", "--no-deps", "bootstrap"], check=False)
        assert result.returncode != 0 and not key.exists()
        backup.rename(key)
        command(compose + ["up", "-d", "--no-build", "--pull", "never", "--wait", "--wait-timeout", "180"])
        ready()
        assert http(secret_path, headers={"Authorization": "Bearer " + mcp_key}, raw=True) == secret
        results["lost_key_refused_and_original_key_restores_secret"] = True
        for name, content in original_config.items():
            assert (config / name).read_bytes() == content
        results["all_generated_config_preserved"] = True
        logs = command(compose + ["logs", "--no-color"]).stdout
        credentials = json.loads((config / "credentials.json").read_text())
        for value in [password, new_password, mcp_key, hook_key, secret.decode(), *credentials.values()]:
            assert value.encode() not in logs, "凭据出现在容器日志中，未输出正文"
        results["no_credentials_in_container_logs"] = True
        result = command(compose + ["stop", "chromie"])
        stopped = command(compose + ["ps", "-a", "--format", "json"]).stdout.decode()
        rows = [json.loads(line) for line in stopped.splitlines() if line.startswith("{")]
        row = next(x for x in rows if x["Service"] == "chromie")
        assert row["ExitCode"] == 0
        results["sigterm_graceful_exit"] = True
    finally:
        if backup.exists() and not key.exists():
            backup.rename(key)
        command(compose + ["down"], check=False)
        (directory / "results.json").write_text(json.dumps(results, ensure_ascii=False, indent=2))
        for path in directory.iterdir():
            if path.is_file():
                path.chmod(0o600)
    print(json.dumps({"results": results, "private_evidence_directory": str(directory)}, ensure_ascii=False))


if __name__ == "__main__":
    main()
