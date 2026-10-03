import shutil
import subprocess
from collections import Counter

from git_procs import SHIM, regressions, render_table, subcommands


def test_subcommands_counts_each_logged_call():
    assert subcommands("ls-files\nhash-object\nhash-object\n\n") == Counter(
        {"hash-object": 2, "ls-files": 1}
    )


def test_the_shim_logs_the_subcommand_and_runs_git(tmp_path):
    shim = tmp_path / "git"
    shim.write_text(SHIM.format(git=shutil.which("git")))
    shim.chmod(0o755)
    log = tmp_path / "git.log"
    env = {"NED_BENCH_GIT_LOG": str(log)}
    for args in [["-C", str(tmp_path), "version"], ["--version"]]:
        out = subprocess.run([shim, *args], env=env, capture_output=True, text=True)
        assert out.stdout.startswith("git version")
    assert log.read_text() == "version\n--version\n"


def test_render_table_lists_the_busiest_subcommands_first():
    table = render_table({"clean": {10: Counter({"rev-parse": 2, "hash-object": 10})}})
    assert table.splitlines()[2] == "| clean | 10 | 12 | hash-object 10, rev-parse 2 |"


def test_regressions_are_counts_above_or_missing_from_the_baseline():
    results = {"clean": {1: Counter(a=18), 10: Counter(a=26)}, "new": {1: Counter(a=1)}}
    baseline = {"clean": {"1": 17, "10": 27}}
    assert regressions(results, baseline) == ["clean 1: 18 > 17", "new 1: 1 > None"]
