"""Token-cost benchmark: ned against sed, Python and str_replace (spec §8).

Runs every variant of every case on a fresh copy of its fixture, checks the
result against the case's expected files, counts each variant's tokens, and
prints the spec's table. With --check, also fails if the spec's table differs.
"""

from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent
TOOLS = ["ned", "sed", "python", "str_replace"]


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
    return []


def apply_str_replace(root: Path, call: str) -> None:
    """Applies a str_replace tool call: `old_str` must occur exactly once."""


def cell(variant: dict[str, str] | None, tokens: int | None) -> str:
    return ""


def spec_table(text: str) -> dict[int, list[str]]:
    """The spec §8 table's cells for each tool, by case number."""
    return {}


def render_table(cases: list[Case], cells: dict[int, list[str]]) -> str:
    return ""


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.parse_args(argv)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
