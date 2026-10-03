"""Git-process benchmark: how many git processes `ned --commit` spawns.

Commits an edit to N files in a fresh repository for each scenario, with a
`git` shim first on PATH that logs every call, and prints the calls by
subcommand. With --check, also fails if a count exceeds git_procs.json's;
--update rewrites that file.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent
BASELINE = ROOT / "git_procs.json"
SIZES = [1, 10, 100]
# How each file stands before the edit: committed and clean, committed with
# a staged change ned must leave staged, or untracked.
SCENARIOS = ["clean", "staged", "untracked"]
# Two lines between the edit and the staged change keep them apart.
TEXT = "x {i}\nkeep\nkeep\n"
SHIM = """\
#!/bin/sh
if [ "$1" = -C ]; then sub=$3; else sub=$1; fi
printf '%s\\n' "$sub" >> "$NED_BENCH_GIT_LOG"
exec {git} "$@"
"""


def subcommands(log: str) -> Counter[str]:
    return Counter(line for line in log.splitlines() if line)


def git(work: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(work), *args], check=True, capture_output=True, text=True
    ).stdout


def setup(work: Path, scenario: str, n: int) -> None:
    """A repository at `work` with N files under ten directories."""
    git(work, "init", "-q")
    for key, value in [
        ("user.name", "Bench"),
        ("user.email", "bench@example.com"),
        ("commit.gpgSign", "false"),
    ]:
        git(work, "config", key, value)
    (work / "README").write_text("bench\n")
    git(work, "add", "README")
    files = [work / f"d{i % 10}" / f"f {i}.txt" for i in range(n)]
    for i, path in enumerate(files):
        path.parent.mkdir(exist_ok=True)
        path.write_text(TEXT.format(i=i))
    if scenario != "untracked":
        git(work, "add", ".")
    git(work, "commit", "-qm", "init")
    if scenario == "staged":
        for path in files:
            path.write_text(path.read_text() + "staged\n")
        git(work, "add", ".")


def run(scenario: str, n: int, env: dict[str, str]) -> Counter[str]:
    """The git calls of one `ned --commit` over N files."""
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp) / "work"
        work.mkdir()
        setup(work, scenario, n)
        log = Path(tmp) / "git.log"
        log.touch()
        result = subprocess.run(
            ["ned", "d*/*.txt", "-e", 'sub /x/ with "y"', "--commit", "m"],
            cwd=work,
            env=env | {"NED_BENCH_GIT_LOG": str(log)},
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            raise RuntimeError(f"{scenario} {n}: {result.stderr.strip()}")
        committed = git(
            work, "diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"
        ).splitlines()
        if len(committed) != n:
            raise RuntimeError(f"{scenario} {n}: committed {len(committed)} files")
        return subcommands(log.read_text())


def render_table(results: dict[str, dict[int, Counter[str]]]) -> str:
    lines = [
        "| Scenario | Files | git processes | By subcommand |",
        "| -------- | ----: | ------------: | ------------- |",
    ]
    for scenario, sizes in results.items():
        for n, calls in sizes.items():
            parts = ", ".join(f"{sub} {count}" for sub, count in calls.most_common())
            lines.append(f"| {scenario} | {n} | {calls.total()} | {parts} |")
    return "\n".join(lines)


def regressions(
    results: dict[str, dict[int, Counter[str]]], baseline: dict[str, dict[str, int]]
) -> list[str]:
    """Each count above the baseline's, or missing from it."""
    found = []
    for scenario, sizes in results.items():
        for n, calls in sizes.items():
            limit = baseline.get(scenario, {}).get(str(n))
            if limit is None or calls.total() > limit:
                found.append(f"{scenario} {n}: {calls.total()} > {limit}")
    return found


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group()
    group.add_argument(
        "--check", action="store_true", help=f"fail on counts above {BASELINE.name}'s"
    )
    group.add_argument("--update", action="store_true", help=f"rewrite {BASELINE.name}")
    args = parser.parse_args(argv)

    subprocess.run(["cargo", "build", "-q", "-p", "ned-cli"], cwd=REPO, check=True)
    real = shutil.which("git")
    if real is None:
        print("git isn't installed", file=sys.stderr)
        return 1
    with tempfile.TemporaryDirectory() as tmp:
        shim = Path(tmp) / "bin" / "git"
        shim.parent.mkdir()
        shim.write_text(SHIM.format(git=real))
        shim.chmod(shim.stat().st_mode | stat.S_IXUSR)
        (Path(tmp) / "config").mkdir()
        env = {k: v for k, v in os.environ.items() if k != "NED_SESSION"} | {
            "PATH": os.pathsep.join(
                [str(shim.parent), str(REPO / "target" / "debug"), os.environ["PATH"]]
            ),
            "XDG_CONFIG_HOME": str(Path(tmp) / "config"),
        }
        results = {s: {n: run(s, n, env) for n in SIZES} for s in SCENARIOS}
    print(render_table(results))

    if args.update:
        counts = {
            s: {str(n): c.total() for n, c in sizes.items()}
            for s, sizes in results.items()
        }
        BASELINE.write_text(json.dumps(counts, indent=2) + "\n")
    if args.check:
        found = regressions(results, json.loads(BASELINE.read_text()))
        for problem in found:
            print(f"{BASELINE.name}: {problem}", file=sys.stderr)
        return 1 if found else 0
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
