"""settings.py against the port (examples/settings_dump.rs) on COPIES of a
settings.json: what load() makes of it, that the port's typed view accepts
it, and a save() with nothing changed - its result, and the file it writes,
byte for byte. Prints counts only; deletes the copies.

    python settings_compare.py <work folder> <settings.json>

Run with sc-watch's build environment, after `cargo build --release -p
scw-store --example settings_dump`.
"""
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
WORK, SRC = Path(sys.argv[1]), Path(sys.argv[2])
sys.path.insert(0, str(ROOT))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
import settings  # noqa: E402

EXE = ".exe" if sys.platform == "win32" else ""
RUST = ROOT / "target" / "release" / "examples" / f"settings_dump{EXE}"


def main():
    WORK.mkdir(parents=True, exist_ok=True)
    read, save_py, save_rs, out = (WORK / "read.json", WORK / "save_py.json",
                                   WORK / "save_rs.json", WORK / "out.json")
    made = [read, save_py, save_rs, out]
    try:
        for p in (read, save_py, save_rs):
            shutil.copyfile(SRC, p)
        loaded = settings.load(read)
        saved = settings.save({}, save_py)
        subprocess.run([str(RUST), str(read), str(save_rs), str(out)], check=True)
        rs = json.loads(out.read_text(encoding="utf-8"))
        problems = []
        if list(loaded.items()) != list(rs["load"].items()):
            n = sum(1 for k in loaded if loaded[k] != rs["load"].get(k))
            problems.append(f"load: {n} settings differ")
        if not rs["typed"]:
            problems.append(f"the typed view refused it: {rs['typed_error']}")
        if list(saved.items()) != list(rs["saved"].items()):
            problems.append("save result")
        if save_py.read_bytes() != save_rs.read_bytes():
            problems.append("the saved files differ")
        changed = sum(1 for k, v in loaded.items() if v != settings.DEFAULTS[k])
        print(f"{len(loaded)} settings, {changed} changed from the default; "
              f"saved file {save_py.stat().st_size} bytes")
        print("IDENTICAL" if not problems else "DIFFERENT: " + "; ".join(problems))
        return 0 if not problems else 1
    finally:
        for p in made:
            try:
                p.unlink()
            except OSError:
                pass


if __name__ == "__main__":
    raise SystemExit(main())
