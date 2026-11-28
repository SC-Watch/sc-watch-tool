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
  licenses/         the licences of the libraries it is built from, and the
                    credit for IRIS, whose name reader it uses (GPL-3.0)

HOW TO RUN

  Start Star Citizen first, then double-click sc-watch in this folder (if
  the file manager asks, choose to run it). It opens a terminal window of
  its own, as sc-watch.exe opens one on Windows: what it says is shown
  there, and closing that window stops it.

  Or, in a terminal in this folder:

      ./sc-watch --verbose

  and Ctrl+C there stops it. Either way the page opens in your browser at
  http://127.0.0.1:8731. Press your read key in game as usual.

THE FIRST START: SHARING THE SCREEN

  On a Wayland desktop (KDE Plasma's default) no program may look at the
  screen unless you allow it. The first time, KDE asks which screen to
  share: pick the monitor the game is on. If the dialog offers to remember
  the choice, allow it; later starts then share the same screen without
  asking. The dialog may name the terminal you started sc-watch from: that
  is the program KDE sees.

  To share a different screen: on the page, Settings > Maintenance >
  "Share a different screen". The desktop asks again.

  If the game runs as an X11 window (Proton's default, not Wine's Wayland
  driver), this also works without the dialog:

      SCW_CAPTURE=x11 ./sc-watch --verbose

KEYS AND JOYSTICKS

  Joystick, throttle and pedal buttons work as they are.

  Keyboard keys need your user in the "input" group: Linux only lets
  programs in that group read keyboards (it is how a keylogger would work,
  so think about whether you want it - any program you run could then read
  keystrokes). The watcher says so when it starts, and so does the page's
  detect button.

  To set it up: on the page, Settings > Maintenance > "Let sc-watch read
  keyboard keys" - "check first", then "set it up". The desktop asks for
  your password in its own dialog (sc-watch never sees it); then log out
  and back in. sc-watch only ever asks whether a key is down: it opens
  every device read-only and never takes a key from the game.

  Or by hand, in a terminal (on Bazzite and other Fedora Atomic systems the
  first line copies the group into /etc/group; elsewhere it is not needed):

      grep -E '^input:' /usr/lib/group | sudo tee -a /etc/group
      sudo usermod -aG input "$USER"

  then log out and back in.

SPEECH AND TONES

  Alerts are spoken by speech-dispatcher (spd-say). If it is missing, the
  watcher says so at the start, and the tones still play.

THE MINING OVERLAY (off until you turn it on)

  On the page: Settings > Mining overlay > on, and a key to pause it if
  you like (e.g. f8 or joy0.b12). Scan a rock in game and a label under the
  signal says what it is, e.g. "Hephaestanite 4x". The game must be in a
  window or borderless, not exclusive fullscreen.

  On KDE Plasma (and Sway, Hyprland) the label is drawn on the desktop's
  overlay layer, over the game, and clicks go through it. GNOME has no such
  layer: there, and wherever no window can open, the labels are written in
  the terminal instead. The table of rocks is data/mining.txt.

YOUR DATA

  This build starts with empty data in data/. Your Windows data folder can
  be copied into data/ if you want your reports here too (the files are the
  same on both systems); close sc-watch first.

CHAT, AND THE OCR READER

  The chat key (Settings > Capture > Chat read key) reads the chat window
  and lists who is talking, with either trigger ('key' or 'auto').

  Settings > Detection > Name reader: 'iris' (the default), or 'ocr', the
  legacy reader, for when the game's own HDR option is on and IRIS reads
  nothing; with it the focus key reads the centre of the screen and the
  dual key reads both. There is no 'iris+ocr': it reads as 'iris'.

HDR

  Nothing to set. With HDR on, KDE Plasma converts the screen to SDR
  itself before sharing it - the picture the game drew, at your SDR
  brightness - which is what sc-watch does for itself on Windows. The
  lines sc-watch prints at the start say whether KDE has HDR on for the
  shared screen. Keep Star Citizen's own HDR option off: it makes HUD text
  bloom (if it is on, Settings > Detection > Name reader 'ocr' reads it).

LOGS, FRAMES AND A SUPPORT BUNDLE

  Every session writes a log to data/debug_bursts/ (the newest 12 are
  kept). With --debug, every burst frame is saved there too, with a line
  per frame saying what IRIS found in it. Housekeeping clears old frames
  and audits by itself (Settings on the page).

  To send them in one go: "make a support bundle" on the page's Settings,
  or

      ./sc-watch --support-bundle

  It writes one zip under 24 MB into data/debug_bursts/ and prints what is
  in it. A full frame is your whole screen - "text only, no screenshots"
  (or --support-bundle --no-frames) leaves them out. Your databases and API
  key are never in it.

WHAT TO SEND BACK

  The lines it prints when it starts (especially the "capture" and
  "screen" ones), whether names are read and announced, and any line
  starting with "!".
