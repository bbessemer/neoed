import pytest

from bench import ROOT, TOOLS, Case, apply_str_replace, cell, load_cases, render_table, spec_table


def test_str_replace_replaces_a_unique_string(tmp_path):
    (tmp_path / "a.rs").write_text("let x = 1;\n")
    apply_str_replace(tmp_path, '{"path": "a.rs", "old_str": "x = 1", "new_str": "y = 2"}')
    assert (tmp_path / "a.rs").read_text() == "let y = 2;\n"


@pytest.mark.parametrize("text", ["let z = 1;\n", "x = 1; x = 1;\n"])
def test_str_replace_needs_exactly_one_occurrence(tmp_path, text):
    (tmp_path / "a.rs").write_text(text)
    with pytest.raises(ValueError):
        apply_str_replace(tmp_path, '{"path": "a.rs", "old_str": "x = 1", "new_str": "y"}')


def test_cells():
    assert cell(None, None) == "—"
    assert cell({"note": "1 call per occurrence"}, None) == "1 call per occurrence"
    assert cell({"command": "sed", "caveat": "unscoped"}, 20) == "20†"
    assert cell({"command": "ned"}, 22) == "22"


SPEC = """\
Some prose.

| #   | Task                            | ned | sed | Python |           str_replace |
| --- | ------------------------------- | --: | --: | -----: | --------------------: |
| 1   | Change a string in one function |  22 | 20† |     70 |                    33 |
| 8   | Rename an identifier in 5 files |  18 |   — |     64 | 1 call per occurrence |

† = not scoped or not reliable.
"""


def test_spec_table_reads_the_cells_by_case():
    assert spec_table(SPEC) == {
        1: ["22", "20†", "70", "33"],
        8: ["18", "—", "64", "1 call per occurrence"],
    }


def test_rendered_tables_read_back():
    cases = [
        Case(1, ROOT, "Change a string in one function", "parser", {}),
        Case(8, ROOT, "Rename an identifier in 5 files", "rename", {}),
    ]
    cells = {1: ["22", "20†", "70", "33"], 8: ["18", "—", "64", "1 call per occurrence"]}
    table = render_table(cases, cells)
    assert table.startswith("| #")
    assert spec_table(table) == cells


def test_every_case_is_complete():
    cases = load_cases(ROOT / "cases")
    assert [c.number for c in cases] == list(range(1, 9))
    for case in cases:
        assert set(case.variants) <= set(TOOLS), case.dir
        assert "command" in case.variants["ned"], case.dir
        assert (ROOT / "fixtures" / case.fixture).is_dir(), case.dir
        assert any((case.dir / "after").rglob("*")), f"{case.dir} has no expected files"
