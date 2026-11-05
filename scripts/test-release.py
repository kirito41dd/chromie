#!/usr/bin/env python3
"""发行边界回归：损坏／错架构不得发布，实例私有资料不得混入发行包。"""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("prepare_release", Path(__file__).with_name("prepare-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
index_spec = importlib.util.spec_from_file_location("publish_index", Path(__file__).with_name("publish-image-index.py"))
index = importlib.util.module_from_spec(index_spec)
index_spec.loader.exec_module(index)


class ImageIndexTests(unittest.TestCase):
    def setUp(self):
        self.image = "docker.io/kirito41dd/chromie"
        self.digests = {"amd64": "sha256:" + "a" * 64, "arm64": "sha256:" + "b" * 64}

    def manifest(self, version, digests=None):
        return {"annotations": {"org.opencontainers.image.version": version}, "manifests": [
            {"platform": {"os": "linux", "architecture": a}, "digest": d}
            for a, d in (digests or self.digests).items()]}

    def test_first_publish_requires_both_digests_then_creates_version_before_latest(self):
        with patch.object(index, "inspect", return_value=None), patch.object(index.subprocess, "run") as run:
            index.publish(self.image, "0.1.0", "f" * 40, self.digests)
            tags = [c.args[0][c.args[0].index("--tag") + 1] for c in run.call_args_list]
            self.assertEqual(tags, [self.image + ":0.1.0", self.image + ":latest"])
            run.reset_mock()
            with self.assertRaises(ValueError):
                index.publish(self.image, "0.1.0", "f" * 40, {"amd64": self.digests["amd64"]})
            run.assert_not_called()

    def test_old_version_cannot_roll_back_latest_and_different_fixed_version_cannot_be_overwritten(self):
        with patch.object(index, "inspect", side_effect=[self.manifest("0.1.0"), self.manifest("0.2.0")]), patch.object(index.subprocess, "run") as run:
            index.publish(self.image, "0.1.0", "f" * 40, self.digests)
            run.assert_not_called()
        wrong = dict(self.digests, arm64="sha256:" + "c" * 64)
        with patch.object(index, "inspect", side_effect=[self.manifest("0.1.0", wrong), None]), patch.object(index.subprocess, "run") as run:
            with self.assertRaises(ValueError):
                index.publish(self.image, "0.1.0", "f" * 40, self.digests)
            run.assert_not_called()

    def test_registry_failures_are_not_treated_as_first_publish(self):
        for error in ["DNS lookup failed", "credential helper not found", "access denied"]:
            with self.subTest(error=error), patch.object(index.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", error)):
                with self.assertRaises(ValueError):
                    index.inspect(self.image + ":latest")
        missing = "ERROR: " + self.image + ":latest: not found\n"
        with patch.object(index.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", missing)):
            self.assertIsNone(index.inspect(self.image + ":latest"))


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.clients = self.root / "clients"
        # 仅构造公开格式的头部夹具；实际可执行性由每个平台的构建 job 另行验证。
        headers = {
            "x86_64-unknown-linux-gnu": b"\x7fELF\x02\x01" + bytes(12) + b"\x3e\x00",
            "aarch64-unknown-linux-gnu": b"\x7fELF\x02\x01" + bytes(12) + b"\xb7\x00",
            "x86_64-apple-darwin": b"\xcf\xfa\xed\xfe\x07\x00\x00\x01",
            "aarch64-apple-darwin": b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01",
        }
        for target, header in headers.items():
            folder = self.clients / target
            folder.mkdir(parents=True)
            binary = folder / "chromie"
            binary.write_bytes(header + b"fictional-client")
            self.checksum(binary)

    def checksum(self, binary):
        binary.with_name("sha256").write_text(hashlib.sha256(binary.read_bytes()).hexdigest() + "\n")

    def test_complete_package_is_pull_only_and_contains_no_extra_private_files(self):
        (self.clients / "private-config.json").write_text('{"key":"fictional-do-not-ship"}')
        output = self.root / "release"
        image = "docker.io/kirito41dd/chromie:" + release.version()
        release.package(output, release.clients(self.clients), image)
        parsed = json.loads(subprocess.check_output([
            "docker", "compose", "-f", str(output / "compose.yaml"), "config", "--format", "json"
        ]))
        for name in ["bootstrap", "chromie"]:
            service = parsed["services"][name]
            self.assertEqual(service["image"], image)
            self.assertNotIn("build", service)
            self.assertNotEqual(service.get("pull_policy"), "never")
        self.assertEqual(len(list(output.glob("*.tar.gz"))), 4)
        for path in output.glob("*.tar.gz"):
            with tarfile.open(path) as archive:
                names = set(archive.getnames())
                self.assertEqual(names, {"chromie", "sha256", *release.LICENSES})
                self.assertTrue(all(m.isfile() and m.uid == m.gid == 0 for m in archive.getmembers()))
                self.assertEqual(archive.getmember("chromie").mode, 0o755)
                self.assertNotIn(b"fictional-do-not-ship", b"".join(archive.extractfile(n).read() for n in names))
        for line in (output / "SHA256SUMS").read_text().splitlines():
            sha, name = line.split("  ")
            self.assertEqual(sha, release.digest(output / name))

    def test_missing_corrupt_wrong_target_and_symlink_are_rejected(self):
        binary = self.clients / "x86_64-unknown-linux-gnu/chromie"
        original = binary.read_bytes()
        for case in ["missing", "corrupt", "wrong-target", "symlink"]:
            with self.subTest(case=case):
                binary.unlink()
                if case == "corrupt":
                    binary.write_bytes(b"corrupt")
                elif case == "wrong-target":
                    binary.write_bytes((self.clients / "aarch64-unknown-linux-gnu/chromie").read_bytes())
                    self.checksum(binary)
                elif case == "symlink":
                    binary.symlink_to(self.clients / "aarch64-unknown-linux-gnu/chromie")
                with self.assertRaises(ValueError):
                    release.clients(self.clients)
                if binary.exists() or binary.is_symlink():
                    binary.unlink()
                binary.write_bytes(original)
                self.checksum(binary)

    def test_version_and_existing_output_are_not_silently_replaced(self):
        current = release.version()
        self.assertEqual(release.version("v" + current), current)
        for tag in [current, "v999.0.0", "v" + current + ";echo unsafe"]:
            with self.assertRaises(ValueError):
                release.version(tag)
        output = self.root / "existing"
        output.mkdir()
        original = output / "private.txt"
        original.write_text("保留原资料")
        with self.assertRaises(ValueError):
            release.package(output, release.clients(self.clients), "docker.io/kirito41dd/chromie:" + current)
        self.assertEqual(original.read_text(), "保留原资料")


if __name__ == "__main__":
    unittest.main()
