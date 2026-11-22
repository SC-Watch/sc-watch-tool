"""watch.Console itself, for tests/console_ops.rs: what reaches the
terminal for a script of announcements and status lines, on a terminal of
a given width and on a redirected output.

    python make_console_ops.py      (sc-watch's build environment)

Writes console_ops.json.
"""
import io
import json
import shutil
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
import watch  # noqa: E402


class Term(io.StringIO):
    def __init__(self, live):
        super().__init__()
        self.live = live

    def isatty(self):
        return self.live


SCRIPT = [
    ["status", "  idle | 0 pings | 0 announced | waiting for full:0"],
    ["say", "[14:13:20]  EXMPLONE                 2.5 km   1/1 votes"],
    ["status", "  reading - captured 3, reading..."],
    ["status", "  reading - reading frame 1/3..."],
    ["say", "          final: EXMPLONE 3/3"],
    ["status", "x" * 120],
    ["say", "line after a clamped status"],
    ["status", "short"],
    ["status", "  watching | 4 frames | 1 contact(s) on screen…"],
    ["done"],
    ["say", "after done"],
    ["status", ""],
    ["say", "after an empty status"],
    ["done"],
    ["done"],
]

results = []
for live, cols in ((True, 80), (True, 40), (True, 3), (False, 80)):
    term = Term(live)
    old = sys.stdout
    shutil_size = shutil.get_terminal_size
    shutil.get_terminal_size = lambda fallback=(100, 24), c=cols: shutil.os.terminal_size((c, 24))
    sys.stdout = term
    try:
        con = watch.Console()
        for op in SCRIPT:
            getattr(con, op[0])(*op[1:])
    finally:
        sys.stdout = old
        shutil.get_terminal_size = shutil_size
    results.append({"live": live, "columns": cols, "out": term.getvalue()})

(HERE / "console_ops.json").write_text(json.dumps({"script": SCRIPT, "runs": results}, indent=1,
                                                  ensure_ascii=False),
                                       encoding="utf-8", newline="\n")
print(len(results), "runs")
