"""Token-cost benchmark: ned against sed, Python and str_replace (spec §8).

Runs every variant of every case on a fresh copy of its fixture, checks the
result against the case's expected files, counts each variant's tokens, and
prints the spec's table. With --check, also fails if the spec's table differs.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent
SPEC = REPO / "docs" / "command-language.md"
TOOLS = ["ned", "sed", "python", "str_replace"]
HEADERS = ["ned", "sed", "Python", "str_replace"]
LANGUAGES = ["rust", "python", "typescript", "tsx", "javascript", "go", "markdown"]
# The spec's counts: tiktoken's o200k_base, as a proxy for LLM tokenizers.
ENCODING = "o200k_base"


@dataclass
class Case:
    number: int
    dir: Path
    task: str
    fixture: str
    # Per tool: `command` (shell) or `call` (str_replace JSON), optional
    # `caveat` (rendered †) or `note` (rendered instead of a count).
    variants: dict[str, dict[str, str]]


def load_cases(cases_dir: Path) -> list[Case]:
    cases = []
    for case_dir in cases_dir.iterdir():
        if not (case_dir / "case.toml").is_file():
            continue
        data = tomllib.loads((case_dir / "case.toml").read_text())
        variants = {tool: data[tool] for tool in TOOLS if tool in data}
        number = int(case_dir.name.split("-", 1)[0])
        cases.append(Case(number, case_dir, data["task"], data["fixture"], variants))
    return sorted(cases, key=lambda c: c.number)


def apply_str_replace(root: Path, call: str) -> None:
    """Applies a str_replace tool call: `old_str` must occur exactly once."""
    args = json.loads(call)
    path = root / args["path"]
    text = path.read_text()
    count = text.count(args["old_str"])
    if count != 1:
        raise ValueError(f"old_str occurs {count} times in {args['path']}")
    path.write_text(text.replace(args["old_str"], args["new_str"]))


def text_of(variant: dict[str, str]) -> str:
    return (variant.get("command") or variant["call"]).rstrip("\n")


def cell(variant: dict[str, str] | None, tokens: int | None) -> str:
    if variant is None:
        return "—"
    if "note" in variant:
        return variant["note"]
    return f"{tokens}{'†' if 'caveat' in variant else ''}"


def spec_table(text: str) -> dict[int, list[str]]:
    """The spec §8 table's cells for each tool, by case number."""
    rows = {}
    for line in text.splitlines():
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if line.startswith("|") and len(cells) == 2 + len(TOOLS) and cells[0].isdigit():
            rows[int(cells[0])] = cells[2:]
    return rows


def render_table(cases: list[Case], cells: dict[int, list[str]]) -> str:
    rows = [[str(c.number), c.task, *cells[c.number]] for c in cases]
    header = ["#", "Task", *HEADERS]
    widths = [max(3, len(h), *(len(r[i]) for r in rows)) for i, h in enumerate(header)]

    def line(values: list[str]) -> str:
        padded = [
            v.ljust(w) if i < 2 else v.rjust(w)
            for i, (v, w) in enumerate(zip(values, widths))
        ]
        return "| " + " | ".join(padded) + " |"

    separator = [
        "-" * w if i < 2 else "-" * (w - 1) + ":" for i, w in enumerate(widths)
    ]
    return "\n".join(
        [line(header), "| " + " | ".join(separator) + " |", *map(line, rows)]
    )


def files_under(root: Path) -> dict[str, str]:
    return {
        str(p.relative_to(root)): p.read_text()
        for p in sorted(root.rglob("*"))
        if p.is_file()
    }


def run(case: Case, tool: str, env: dict[str, str]) -> str | None:
    """Runs one variant on a copy of the fixture; returns what went wrong."""
    variant = case.variants[tool]
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp) / "work"
        shutil.copytree(ROOT / "fixtures" / case.fixture, work)
        if tool == "str_replace":
            try:
                apply_str_replace(work, variant["call"])
            except ValueError as err:
                return str(err)
        else:
            result = subprocess.run(
                ["bash", "-c", variant["command"]],
                cwd=work,
                env=env,
                capture_output=True,
                text=True,
            )
            if result.returncode != 0:
                first = (result.stderr.strip().splitlines() or [""])[0]
                return f"exit {result.returncode}: {first}"
        expected = files_under(ROOT / "fixtures" / case.fixture) | files_under(
            case.dir / "after"
        )
        actual = files_under(work)
        wrong = sorted(
            p
            for p in expected.keys() | actual.keys()
            if expected.get(p) != actual.get(p)
        )
        return (
            f"differs from the expected result in {', '.join(wrong)}" if wrong else None
        )


def bsd_sed() -> bool:
    """Whether `sed` takes BSD syntax (`sed -i ''`), as the sed variants use."""
    return subprocess.run(["sed", "--version"], capture_output=True).returncode != 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check", action="store_true", help="fail if the spec's table differs"
    )
    args = parser.parse_args(argv)

    import tiktoken

    subprocess.run(["cargo", "build", "-q", "-p", "ned-cli"], cwd=REPO, check=True)
    encoding = tiktoken.get_encoding(ENCODING)
    failed = False
    with tempfile.TemporaryDirectory() as config:
        # Formatters would reformat the fixtures beyond each edit.
        (Path(config) / "ned").mkdir()
        (Path(config) / "ned" / "config.toml").write_text(
            "[format]\n" + "".join(f"{lang} = false\n" for lang in LANGUAGES)
        )
        env = os.environ | {
            "PATH": f"{REPO / 'target' / 'debug'}{os.pathsep}{os.environ['PATH']}",
            "XDG_CONFIG_HOME": config,
        }
        cases = load_cases(ROOT / "cases")
        cells = {}
        for case in cases:
            row = []
            for tool in TOOLS:
                variant = case.variants.get(tool)
                if variant is None or "note" in variant:
                    row.append(cell(variant, None))
                    continue
                row.append(cell(variant, len(encoding.encode(text_of(variant)))))
                if tool == "sed" and not bsd_sed():
                    print(
                        f"case {case.number} sed: not run (needs BSD sed)",
                        file=sys.stderr,
                    )
                    continue
                problem = run(case, tool, env)
                if problem and "caveat" in variant:
                    print(f"case {case.number} {tool} (†): {problem}", file=sys.stderr)
                elif problem:
                    print(f"case {case.number} {tool}: {problem}", file=sys.stderr)
                    failed = True
            cells[case.number] = row
    print(render_table(cases, cells))

    if args.check:
        spec = spec_table(SPEC.read_text())
        for number, row in cells.items():
            if spec.get(number) != row:
                print(
                    f"spec §8 row {number}: {spec.get(number)} != {row}",
                    file=sys.stderr,
                )
                failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
