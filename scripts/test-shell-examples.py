#!/usr/bin/env python3
"""只解析文档中的 Shell 代码块，不执行建库、密码或服务命令。"""
from pathlib import Path
import re
import shutil
import subprocess
import sys

root = Path(__file__).resolve().parent.parent
nu = shutil.which("nu")
if not nu:
    sys.exit("需要已安装的 Nushell；本脚本不会自动安装依赖。")
paths = [root / "AGENTS.md", root / "README.md", *root.glob("src/*_guide.md"),
         *root.glob("docs/*.md"), *root.glob("integrations/**/*.md"), *root.glob("examples/**/*.md")]
count = {"bash": 0, "nu": 0}
for path in paths:
    for match in re.finditer(r"```(bash|sh|shell|nu)\n(.*?)\n```", path.read_text(), re.S):
        kind, body = match.groups()
        shells = ["bash", "nu"] if kind == "shell" else ["nu" if kind == "nu" else "bash"]
        for shell in shells:
            command = ["bash", "-n"] if shell == "bash" else [nu, "--no-config-file", "--stdin", "-c", "if not ($in | nu-check --debug) { exit 1 }"]
            result = subprocess.run(command, cwd=root, input=body, capture_output=True, text=True)
            if result.returncode:
                print(f"语法检查未通过：{path.relative_to(root)} 第 {match.start()} 字符处（{shell}）", file=sys.stderr)
                print(result.stderr, file=sys.stderr)
                sys.exit(1)
            count[shell] += 1
print(f"只读语法验证通过：Bash {count['bash']} 块，Nushell {count['nu']} 块。")
