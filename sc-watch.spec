# -*- mode: python ; coding: utf-8 -*-
"""
PyInstaller spec for sc-watch. Built by build.py, not by hand.

TWO EXECUTABLES, ONE FOLDER
---------------------------
The watcher and the UI are separate processes on purpose - either can be
restarted without disturbing the other, and the watcher owns the capture
device's lifecycle. That property is worth keeping in a packaged build, so
this produces two exes that share one `_internal` folder of libraries rather
than two independent bundles that would each carry a 45 MB copy of onnxruntime.

ONE-FOLDER, NOT ONE-FILE
------------------------
A one-file build self-extracts about a quarter of a gigabyte to a temporary
directory on EVERY launch. Measured cold start from source is 0.52s; unpacking
that much would dominate it. Self-extracting archives are also the shape most
likely to trip antivirus heuristics, and an unsigned game utility does not need
the extra suspicion.

WHAT HAS TO BE COLLECTED BY HAND
--------------------------------
PyInstaller follows imports. It cannot follow a path built at runtime, and
three of our dependencies load their real payload that way:

  rapidocr_onnxruntime  reads config.yaml and the three .onnx models from
                        inside its own package directory. Without them the
                        engine imports fine and then fails at first use, which
                        is the worst time to find out.
  onnxruntime           loads onnxruntime.dll through its capi subpackage.
  dxcam                 reaches DXGI through comtypes, which GENERATES python
                        modules at runtime; the generated package has to be
                        present or capture fails on a machine that has never
                        run it from source.
"""

from pathlib import Path

from PyInstaller.utils.hooks import collect_data_files, collect_submodules

HERE = Path(SPECPATH)

# Our own read-only resources. paths.resource() looks for these next to the
# executable, which is where a one-folder build puts datas.
datas = [
    (str(HERE / "km_template.npy"), "."),
    (str(HERE / "tones"), "tones"),
]

# The OCR engine's models and configs.
datas += collect_data_files("rapidocr_onnxruntime", include_py_files=False)

hiddenimports = [
    "onnxruntime",
    "onnxruntime.capi",
    "onnxruntime.capi._pybind_state",
    # dxcam builds its COM interfaces through comtypes at import time.
    "comtypes",
    "comtypes.stream",
    # Optional at runtime, but if it is installed at build time we want the
    # speech path to work in the built app rather than silently degrade.
    "pyttsx3",
    "pyttsx3.drivers",
    "pyttsx3.drivers.sapi5",
]
hiddenimports += collect_submodules("rapidocr_onnxruntime")

# Weight. Everything below is either a GUI toolkit we do not use or a science
# stack numpy drags in by name. Excluding them is 60-80 MB off the download and
# removes DLLs that have nothing to do with reading text off a HUD.
excludes = [
    "tkinter", "PyQt5", "PyQt6", "PySide2", "PySide6", "wx",
    "matplotlib", "scipy", "pandas", "IPython", "jupyter",
    "notebook", "pytest", "setuptools", "pip",
]

_common = dict(pathex=[str(HERE)], binaries=[], datas=datas,
               hiddenimports=hiddenimports, hookspath=[], hooksconfig={},
               runtime_hooks=[], excludes=excludes, noarchive=False)

watcher = Analysis([str(HERE / "watch.py")], **_common)
server = Analysis([str(HERE / "ui_server.py")], **_common)

# MERGE makes the second executable reference the first one's copy of every
# shared library instead of duplicating it. Without this the folder carries
# onnxruntime, OpenCV and numpy twice.
MERGE((watcher, "watch", "sc-watch"), (server, "ui_server", "sc-watch-ui"))

watcher_pyz = PYZ(watcher.pure)
server_pyz = PYZ(server.pure)

watcher_exe = EXE(
    watcher_pyz, watcher.scripts, [],
    exclude_binaries=True,
    name="sc-watch",
    debug=False, bootloader_ignore_signals=False, strip=False, upx=False,
    # console=True on purpose. The watcher prints its trigger bindings, what it
    # read, and the session summary; a windowed build would throw all of that
    # away and leave a tool whose whole feedback channel is invisible.
    console=True,
    disable_windowed_traceback=False, argv_emulation=False,
    icon=str(HERE / "icon.ico") if (HERE / "icon.ico").exists() else None,
)

server_exe = EXE(
    server_pyz, server.scripts, [],
    exclude_binaries=True,
    name="sc-watch-ui",
    debug=False, bootloader_ignore_signals=False, strip=False, upx=False,
    console=True,
    disable_windowed_traceback=False, argv_emulation=False,
    icon=str(HERE / "icon.ico") if (HERE / "icon.ico").exists() else None,
)

COLLECT(
    watcher_exe, watcher.binaries, watcher.datas,
    server_exe, server.binaries, server.datas,
    strip=False, upx=False, upx_exclude=[], name="sc-watch",
)
