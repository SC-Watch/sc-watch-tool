@echo off
REM Launch sc-watch: the UI on one window, the watcher on another.
REM
REM They are separate processes on purpose - the watcher writes to SQLite and
REM the UI reads from it, so either can be restarted without disturbing the
REM other. Close a window to stop that half.

setlocal
cd /d "%~dp0"

REM Prefer the launcher; fall back to whatever python is on PATH.
where py >nul 2>nul && (set PY=py -3) || (set PY=python)

echo ==========================================================
echo   sc-watch
echo   UI      : http://127.0.0.1:8731
echo   watcher : press 0 in game to read contacts
echo   close either window to stop that half
echo ==========================================================
echo.

REM No flags here on purpose. Everything the watcher needs now comes from
REM settings.json, edited in the UI's Settings tab - the launcher used to
REM hardcode --debug, which quietly overrode the setting and let
REM debug_bursts/ reach 1.1 GB. Anything passed to this script is still
REM forwarded, so "sc-watch.bat --debug" works for a one-off session.
start "sc-watch UI" %PY% ui_server.py
timeout /t 2 /nobreak >nul
start "sc-watch watcher" %PY% watch.py --verbose %*

endlocal
