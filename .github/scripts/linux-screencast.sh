#!/usr/bin/env bash
# The Wayland capture's tests on CI's Ubuntu runner, against real programs:
#
# 1. spa_oracle: spa.rs's pods against SPA's own builder (C, the system's
#    headers).
# 2. pipewire_stream: a PipeWire daemon and session manager, and
#    GStreamer's videotestsrc into a node of one colour, read by the stream
#    reader.
# 3. portal_sway: a headless sway (one output, one colour), the
#    screen-cast portal with its wlroots backend told to share that output
#    without a dialog, and the whole ScreenCastSource.
# 4. overlay_sway: the mining overlay's window (scw-overlay) on that sway,
#    a layer-shell surface, checked in grim's screenshots of the output.
#
# Each test says on the run whether it ran (::notice) or why not
# (::warning). Setting up is best effort - a part that will not start makes
# its test skip with a warning - but a test that runs and fails fails the
# step, with its message and the programs' logs as annotations (readable
# without signing in, unlike the log).
set -uo pipefail

note() { echo "::warning title=screen-cast setup::$*"; }

# Up to 10 s for a check to pass; a warning when it does not.
wait_for() { # what, then the check
  local what=$1
  shift
  for _ in $(seq 50); do
    "$@" && return 0
    sleep 0.2
  done
  note "$what did not come up in 10 s"
  return 1
}

# Does someone own this name on the session bus? (Asking the bus itself
# starts nothing; asking a name it can activate would.)
has_name() {
  dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus \
    org.freedesktop.DBus.ListNames 2>/dev/null | grep -q "\"$1\""
}

# Does the running portal offer screen casting?
has_screencast() {
  has_name org.freedesktop.portal.Desktop \
    && dbus-send --session --print-reply --dest=org.freedesktop.portal.Desktop \
      /org/freedesktop/portal/desktop org.freedesktop.DBus.Introspectable.Introspect 2>/dev/null \
    | grep -q 'org.freedesktop.portal.ScreenCast'
}

# A file's last lines as one annotation.
show() { # title, file, level
  [ -s "$2" ] || return 0
  echo "::${3:-warning} title=$1::$(tail -n 45 "$2" | cut -c1-240 | sed 's/%/%25/g' | awk '{printf "%s%%0A", $0}')"
}

sudo apt-get install -y -qq \
  pkg-config libspa-0.2-dev pipewire wireplumber \
  gstreamer1.0-pipewire gstreamer1.0-plugins-base gstreamer1.0-tools \
  sway swaybg xdg-desktop-portal xdg-desktop-portal-wlr grim gdb >/dev/null \
  || note "some packages did not install"

logs="$RUNNER_TEMP/screencast-logs"
mkdir -p "$logs"
failed=0
run_test() { # name, then env assignments for the test
  local name=$1
  shift
  if ! env "$@" cargo test -q -p scw-capture --test "$name" -- --nocapture 2>&1 | tee "$logs/test-$name.out"; then
    failed=1
    show "$name failed" "$logs/test-$name.out" error
    return 1
  fi
}

# 1. The pods.
oracle="$RUNNER_TEMP/spa_oracle"
if cc -o "$oracle" crates/scw-capture/tests/spa_oracle.c $(pkg-config --cflags libspa-0.2) 2>"$logs/cc.log"; then
  run_test spa_oracle SCW_SPA_ORACLE="$oracle"
else
  show "the SPA reference program did not build" "$logs/cc.log"
  run_test spa_oracle
fi

# A private session: runtime dir, D-Bus, PipeWire, WirePlumber.
export XDG_RUNTIME_DIR="$RUNNER_TEMP/xdg-runtime"
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
export XDG_CONFIG_HOME="$RUNNER_TEMP/xdg-config"
mkdir -p "$XDG_CONFIG_HOME"
if addr=$(dbus-daemon --session --fork --print-address 2>"$logs/dbus.log"); then
  export DBUS_SESSION_BUS_ADDRESS=$addr
else
  show "no D-Bus session" "$logs/dbus.log"
fi
pipewire >"$logs/pipewire.log" 2>&1 &
sleep 1
wireplumber >"$logs/wireplumber.log" 2>&1 &
sleep 2

# 2. A video node of one colour (0xAARRGGBB: red 20, green 80, blue c0).
gst-launch-1.0 -q videotestsrc pattern=solid-color foreground-color=0xff2080c0 is-live=true \
  ! video/x-raw,format=BGRx,width=320,height=240,framerate=30/1 \
  ! pipewiresink mode=provide \
    stream-properties='props,media.class="Video/Source",node.name=scwtestsrc' \
  >"$logs/gst.log" 2>&1 &
gst=$!
sleep 2
if kill -0 $gst 2>/dev/null; then
  if ! run_test pipewire_stream SCW_PW_TEST_SOURCE=scwtestsrc; then
    pw-cli ls Node >"$logs/nodes.log" 2>&1
    show "PipeWire's nodes" "$logs/nodes.log"
    show "pipewire" "$logs/pipewire.log"
    show "wireplumber" "$logs/wireplumber.log"
    show "gst" "$logs/gst.log"
  fi
else
  show "GStreamer's test source did not start" "$logs/gst.log"
  run_test pipewire_stream
fi
kill $gst 2>/dev/null

# 3. A headless Wayland desktop and the portal.
colour=3070b0
printf 'xwayland disable\noutput * bg #%s solid_color\n' "$colour" >"$RUNNER_TEMP/sway.conf"
mkdir -p "$XDG_CONFIG_HOME/xdg-desktop-portal-wlr" "$XDG_CONFIG_HOME/xdg-desktop-portal"
printf '[screencast]\nchooser_type=none\noutput_name=HEADLESS-1\n' \
  >"$XDG_CONFIG_HOME/xdg-desktop-portal-wlr/config"
printf '[preferred]\ndefault=wlr\n' >"$XDG_CONFIG_HOME/xdg-desktop-portal/sway-portals.conf"
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
  sway -c "$RUNNER_TEMP/sway.conf" >"$logs/sway.log" 2>&1 &
sway=$!
socket=""
for _ in $(seq 50); do
  socket=$(ls "$XDG_RUNTIME_DIR" 2>/dev/null | grep -m1 '^wayland-[0-9]*$')
  [ -n "$socket" ] && break
  sleep 0.2
done
if [ -n "$socket" ] && kill -0 $sway 2>/dev/null; then
  export WAYLAND_DISPLAY=$socket XDG_CURRENT_DESKTOP=sway
  sleep 1
  /usr/libexec/xdg-desktop-portal-wlr >"$logs/xdpw.log" 2>&1 &
  # Waited for, not slept on: the portal looks for its backends when it
  # starts, so a backend that registers late leaves it without a screen-cast
  # interface for good (seen once on CI: "No such interface ...ScreenCast").
  portal_up=1
  if command -v dbus-send >/dev/null; then
    wait_for "the wlroots portal backend" has_name org.freedesktop.impl.portal.desktop.wlr \
      || portal_up=0
    /usr/libexec/xdg-desktop-portal -r >"$logs/xdp.log" 2>&1 &
    wait_for "the screen-cast portal" has_screencast || portal_up=0
  else
    sleep 1
    /usr/libexec/xdg-desktop-portal -r >"$logs/xdp.log" 2>&1 &
    sleep 2
  fi
  if [ $portal_up = 0 ]; then
    show "xdg-desktop-portal-wlr" "$logs/xdpw.log"
    show "xdg-desktop-portal" "$logs/xdp.log"
    run_test portal_sway
  elif ! run_test portal_sway SCW_PORTAL_TEST=$colour; then
    # A crash: the test again under gdb, for where.
    bin=$(grep -o "process didn't exit successfully: .[^ ]*" "$logs/test-portal_sway.out" | sed 's/.*`//')
    if [ -n "$bin" ] && command -v gdb >/dev/null; then
      SCW_PORTAL_TEST=$colour timeout 120 gdb -q -batch -ex run -ex bt -ex "thread apply all bt 8" \
        --args "$bin" --nocapture >"$logs/gdb.log" 2>&1
      grep -A 45 "received signal" "$logs/gdb.log" | head -n 45 >"$logs/gdb-crash.log"
      show "portal_sway under gdb" "$logs/gdb-crash.log"
    fi
    show "sway" "$logs/sway.log"
    show "xdg-desktop-portal-wlr" "$logs/xdpw.log"
    show "xdg-desktop-portal" "$logs/xdp.log"
  fi
  # 4. The overlay window on the same desktop.
  if command -v grim >/dev/null; then
    if SCW_OVERLAY_TEST=1 cargo test -q -p scw-overlay --test overlay_sway -- --nocapture 2>&1 \
        | tee "$logs/test-overlay_sway.out"; then
      echo "::notice title=overlay_sway::$(grep -c '^step:' "$logs/test-overlay_sway.out") steps checked"
    else
      failed=1
      show "overlay_sway failed" "$logs/test-overlay_sway.out" error
      show "sway" "$logs/sway.log"
    fi
  else
    note "grim did not install: the overlay window was not checked"
  fi
else
  show "sway did not start" "$logs/sway.log"
  run_test portal_sway
fi
exit $failed
