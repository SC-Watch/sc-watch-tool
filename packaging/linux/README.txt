sc-watch - Rust test build for Linux (branch rust-port, commit @COMMIT@)
======================================================================

Nothing to install: this folder is the whole program. Unpack it anywhere
in your home folder and run it from there. It works the same on Bazzite
and other immutable systems.

WHAT IS HERE

  sc-watch          the watcher AND the page, in one window. Start this one.
  sc-watch-watcher  the watcher alone
  sc-watch-ui       the page alone
  portable.txt      keeps everything it saves in data/ beside the programs
  starter_list.json the starter watchlist
  licenses/         the licences of the libraries it is built from

HOW TO RUN

  Start Star Citizen first, then, in a terminal in this folder:

      ./sc-watch --verbose

  The page opens in your browser at http://127.0.0.1:8731. Press your read
  key in game as usual. Ctrl+C in the terminal stops it.

THE FIRST START: SHARING THE SCREEN

  On a Wayland desktop (KDE Plasma's default) no program may look at the
  screen unless you allow it. The first time, KDE asks which screen to
  share: pick the monitor the game is on. If the dialog offers to remember
  the choice, allow it; later starts then share the same screen without
  asking. The dialog may name the terminal you started sc-watch from: that
  is the program KDE sees.

  To be asked again, delete data/screencast_token.txt.

  If the game runs as an X11 window (Proton's default, not Wine's Wayland
  driver), this also works without the dialog:

      SCW_CAPTURE=x11 ./sc-watch --verbose

KEYS AND JOYSTICKS

  Joystick, throttle and pedal buttons work as they are.

  Keyboard keys need your user in the "input" group: Linux only lets
  programs in that group read keyboards (it is how a keylogger would work,
  so think about whether you want it). The watcher says so when it starts.
  On Bazzite and other Fedora Atomic systems the group must first be copied
  into /etc/group:

      grep -E '^input:' /usr/lib/group | sudo tee -a /etc/group
      sudo usermod -aG input "$USER"

  then log out and back in.

SPEECH AND TONES

  Alerts are spoken by speech-dispatcher (spd-say). If it is missing, the
  watcher says so at the start, and the tones still play.

YOUR DATA

  This build starts with empty data in data/. Your Windows data folder can
  be copied into data/ if you want your reports here too (the files are the
  same on both systems); close sc-watch first.

NOT IN THIS VERSION YET

  - OCR: reading chat, focused reads and the "iris+ocr" reader. The read,
    focus and dual keys all do a full IRIS read.
  - HDR: frames are read as the desktop shares them. With HDR on in KDE,
    say how the names read.
  - --support-bundle and --check-resolution.

WHAT TO SEND BACK

  The lines it prints when it starts (especially the "capture" and
  "screen" ones), whether names are read and announced, and any line
  starting with "!".
