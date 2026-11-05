#!/usr/bin/env python3
"""通过本机虚构下载源验证平台选择与失败时保留旧程序，不安装到真实 HOME。"""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
import shutil
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

INSTALLER = Path(__file__).resolve().parent.parent / "integrations/install.sh"


class InstallerTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="chromie-install-test-")
        self.root = Path(self.temporary.name)
        self.bin = self.root / ".local/bin/chromie"
        self.bin.parent.mkdir(parents=True)
        self.bin.write_text("原有程序")
        self.body = b'#!/bin/sh\n[ "$1" = help ]\n'
        self.checksum = None
        self.missing = False
        self.paths = []
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                fixture.paths.append(self.path)
                if fixture.missing:
                    self.send_error(404)
                    return
                data = fixture.body if self.path.endswith("/chromie") else (
                    fixture.checksum or hashlib.sha256(fixture.body).hexdigest()
                ).encode()
                self.send_response(200)
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *_args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.origin = f"http://127.0.0.1:{self.server.server_port}"

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.temporary.cleanup()

    def run_installer(self, system="Darwin", arch="arm64"):
        tools = self.root / "tools"
        tools.mkdir(exist_ok=True)
        uname = tools / "uname"
        uname.write_text(f'#!/bin/sh\ncase "$1" in -s) echo {system};; -m) echo {arch};; esac\n')
        uname.chmod(0o700)
        env = {**os.environ, "HOME": str(self.root), "PATH": str(tools) + ":" + os.environ["PATH"]}
        return subprocess.run(["sh", str(INSTALLER), self.origin], env=env, capture_output=True, timeout=15)

    def test_native_platform_selection_and_verified_replacement(self):
        for system, arch, target in [
            ("Darwin", "arm64", "aarch64-apple-darwin"),
            ("Darwin", "x86_64", "x86_64-apple-darwin"),
            ("Linux", "x86_64", "x86_64-unknown-linux-gnu"),
            ("Linux", "aarch64", "aarch64-unknown-linux-gnu"),
        ]:
            with self.subTest(target=target):
                self.paths.clear()
                self.assertEqual(self.run_installer(system, arch).returncode, 0)
                self.assertEqual(self.bin.read_bytes(), self.body)
                self.assertEqual(self.bin.stat().st_mode & 0o777, 0o700)
                self.assertEqual(self.paths, [f"/integrations/cli/{target}/sha256", f"/integrations/cli/{target}/chromie"])

    @unittest.skipUnless(shutil.which("nu"), "需要本机已安装的 Nushell")
    def test_nushell_install_pipeline_and_path(self):
        tools = self.root / "tools"
        tools.mkdir()
        uname = tools / "uname"
        uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
        uname.chmod(0o700)
        env = {**os.environ, "HOME": str(self.root), "PATH": str(tools) + ":" + os.environ["PATH"]}
        command = f"^curl -fsS '{self.origin}/install.sh' | ^sh -s -- '{self.origin}'"
        # 安装脚本也由这个虚构服务提供；不用真实安装位置。
        original = self.server.RequestHandlerClass.do_GET
        def serve(handler):
            if handler.path == "/install.sh":
                handler.send_response(200); handler.end_headers(); handler.wfile.write(INSTALLER.read_bytes())
            else:
                original(handler)
        self.server.RequestHandlerClass.do_GET = serve
        command += "; $env.PATH = ($env.PATH | prepend ($nu.home-dir | path join '.local' 'bin') | uniq); ^($nu.home-dir | path join '.local' 'bin' 'chromie') help"
        result = subprocess.run([shutil.which("nu"), "--no-config-file", "-c", command], env=env, capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        self.assertEqual(self.bin.read_bytes(), self.body)
        self.assertIn("Nushell", result.stdout.decode())
        self.assertIn("chromie path --shell nushell", result.stdout.decode())

    def test_checksum_failure_preserves_existing_program(self):
        self.checksum = "0" * 64
        self.assertNotEqual(self.run_installer().returncode, 0)
        self.assertEqual(self.bin.read_text(), "原有程序")

    def test_missing_artifact_and_unsupported_platform_preserve_existing_program(self):
        self.missing = True
        self.assertNotEqual(self.run_installer().returncode, 0)
        self.assertEqual(self.bin.read_text(), "原有程序")
        self.paths.clear()
        self.assertNotEqual(self.run_installer("Unknown", "unknown").returncode, 0)
        self.assertEqual(self.paths, [])
        self.assertEqual(self.bin.read_text(), "原有程序")

    def test_incompatible_executable_is_not_installed(self):
        self.body = b"#!/bin/sh\nexit 42\n"
        self.assertNotEqual(self.run_installer().returncode, 0)
        self.assertEqual(self.bin.read_text(), "原有程序")

    def test_http_gateways_do_not_depend_on_ip_location(self):
        real_origin = self.origin
        tools = self.root / "tools"
        tools.mkdir(exist_ok=True)
        curl = tools / "curl"
        import sys
        curl.write_text(f'#!{sys.executable}\nimport os,sys,urllib.parse\na=[{real_origin!r}+urllib.parse.urlsplit(x).path if x.startswith("http://") else x for x in sys.argv[1:]]\nos.execv({shutil.which("curl")!r}, ["curl", *a])\n')
        curl.chmod(0o700)
        for origin in ["http://192.0.2.10:8732", "http://public.example:8732", "http://8.8.8.8:8732"]:
            self.origin = origin
            self.paths.clear()
            self.assertEqual(self.run_installer().returncode, 0)
            self.assertEqual(len(self.paths), 2)
        self.origin = "ftp://public.example:8732"
        self.paths.clear()
        self.assertNotEqual(self.run_installer().returncode, 0)
        self.assertEqual(self.paths, [])


if __name__ == "__main__":
    unittest.main()
