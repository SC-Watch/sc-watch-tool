# sc-watch

Reads player handles off the Star Citizen flight HUD after a ping, checks them
against a local reputation database and RSI org data, announces them by audio,
and shows them as tiles in a local web page.

Windows only. Python 3.11 or newer.

---

## Is this safe to run?

It is read-only and entirely out-of-process:

- It captures the desktop through **DXGI Desktop Duplication**, the same API a
  screen recorder uses. It never reads or writes the game's memory.
- It **draws no overlay**. Nothing is injected into the render pipeline.
- It **sends no input**. Keys are read with `GetAsyncKeyState`, which polls
  what is already held. No hook is installed and no keystroke is swallowed, so
  the game receives everything you press exactly as you pressed it.
- Joysticks are read through the legacy **winmm** API, which is also polling.

That boundary is deliberate. Overlays that inject into the render pipeline and
tools that synthesise input are what anti-cheat systems care about; this does
neither. It is a screen reader that happens to be pointed at a game.

It is not affiliated with or endorsed by Cloud Imperium Games.

---

## Install

**Download the installer from the [Releases page](../../releases)** and run it.
No Python, no dependencies, no administrator prompt: it installs for your user
account only.

Windows SmartScreen will warn about an unrecognised publisher. That is because
this build is not code-signed, which costs money and an organisational
identity. Choose *More info*, then *Run anyway*. If you would rather not, use
the zip instead.

**Prefer no installer?** Take the `.zip` from the same page, unpack it
anywhere, and run `sc-watch.bat`. Put an empty file named `portable.txt` beside
the executables and it keeps all its data in that folder instead of your user
profile, which is what you want on a USB stick or in a folder you sync
yourself.

**Running from source instead:**

```bash
pip install -r requirements.txt
python watch.py
```

Python 3.11 or newer. The OCR models come with `rapidocr-onnxruntime` and need
no separate download.

### Where your data goes

```bash
sc-watch.exe --where
```

Installed, that is `%LOCALAPPDATA%\sc-watch` — your database, settings, profile
cache and audit screenshots, kept out of the install directory so an upgrade or
an uninstall cannot take them with it. From source, everything stays beside the
scripts exactly as it always has.

### Optional: an API key

A free key from starcitizen-api.com adds org and profile data. Put it in
`sc_api_key.txt` inside your data folder, or set `SC_API_KEY`.

Without a key the tool still reads handles, records sightings and speaks
alerts. What you lose is which org someone flies for, and the check that stops
two similar-looking names being merged into one person.

---

## Run it

```bash
sc-watch.bat
```

Installed, that is the Start Menu shortcut. From the zip or from source, run
`sc-watch.bat` in the folder.

Either way it opens the UI at `http://127.0.0.1:8731` and starts the watcher in
a second window. They are two processes on purpose, so either can be restarted
without disturbing the other. Close a window to stop that half.

In game, ping, then press **`0`**. Contacts appear as tiles within a second or
two and are announced by audio. Every key is rebindable in Settings, including
joystick buttons and combinations like `rctrl + joy0.b24`.

---

## Two settings in the game itself

These matter more than anything in this tool's own configuration.

1. **Turn chromatic aberration OFF.** This one is close to non-negotiable.
   With it on, the coloured fringing on HUD text defeats the recogniser and it
   reads roughly one label in ten.
2. **Turn the debug overlay off** (`r_displayInfo 0`). Not fatal, but it costs
   about a third of the per-frame OCR budget, and that budget is what decides
   whether the contact you are actually pointing at gets read in time.

---

## If it reads nothing

Run the resolution check against a screenshot of your own HUD:

```bash
python check_resolution.py myscreenshot.png
```

It reports, in the order things break: whether the image is letterboxed,
whether the brightness locators find any HUD text at all, how tall your labels
are in pixels, and what the detector made of the frame. Everything about the
geometry was calibrated on a 5120x1440 display and scales from there, so a
very different resolution is the first thing worth ruling out.

If labels are plainly visible but never reported, turn on **Re-read dim range
lines** in Settings. Some displays render the range line under a name dimmer
than the name itself, and one global brightness threshold then keeps half of a
label and discards the contact.

---

## Your data stays on your machine

Everything is local. There is no account, no telemetry and no server.

In your data folder, which `sc-watch.exe --where` will point you at:

- `sc-watch.db` holds sightings, reports and orgs.
- `rsi_cache.db` caches profile lookups so the same handle is not fetched
  twice.
- `audit/` holds cropped screenshots of labels you asked to check by eye.
- `debug_bursts/` and `live_frames/` hold captured frames, if you turn those
  on. They are large and swept on a retention policy you control in Settings.

The only outbound requests are profile lookups to starcitizen-api.com, and
only when you have supplied a key.

**None of those files are in this repository, and none of them should ever be
committed.** They are records about named real people. `.gitignore` covers
them; check before you push.

The web UI binds to `127.0.0.1` and refuses any request that does not arrive
with a loopback `Host` header, so another machine cannot reach it and another
website open in your browser cannot drive it.

---

## Reporting someone

A report is a claim about something a person did, with a category, a note and
a date. The tool keeps those separate from facts like "this handle belongs to
this org", and it will not invent one from the other.

It will also refuse to merge two spellings that both resolve to real RSI
accounts, because `SLIVER` and `5LIVER` are two different people and giving
one of them the other's history is the worst mistake this tool could make.
That check needs an API key. Without one, a merge you confirm by hand goes
through unverified and the UI says so.

## Flagging an org

Settings has a Flagged orgs panel. Flag an org by its SID and every member of
it is marked the first time you meet them, without writing a report against
anybody.

A flag on its own reaches almost nobody, so the panel pulls the org's member
list when you flag it. The reason is that the profile lookup returns a player's
main org only, and a piracy side-org is nearly always an affiliate, which never
shows up there. The member list is the only route to those people. Each org in
the panel says how many members it has, when the list was last pulled, and how
many contacts you have actually seen that it reaches.

Pulling a list is one request per second and up to twenty pages, so a large org
takes a while. It runs in the background and nothing pulls one on a timer.

This records membership, not blame. No report is written against anyone.
Unflagging an org removes the membership rows it added.

---

## Licence

sc-watch is free software under the **GNU General Public License v3.0**. The
full text is in [LICENSE](LICENSE).

In short: use it, study it, change it, share it. If you distribute a modified
version, you must publish your source under the same terms. Nobody can take
this closed, wrap it in a paid product, and keep the changes to themselves.

What the licence deliberately does **not** do is restrict *use*. No open-source
licence can, and this one does not try. Anyone may run it for any purpose. The
limits on misuse are in the design instead: it refuses to fold two handles onto
each other when both resolve to real accounts, and it records org membership as
a fact separate from any claim about what a person did.

It also comes with no warranty. This tool reads blurry text off a screen and is
sometimes wrong about a name. Treat what it tells you accordingly.

Third-party components shipped alongside it keep their own licences, all
permissive and all compatible with GPL-3.0. They are listed with their full
texts in `THIRD-PARTY-NOTICES.txt`, generated at build time from the packages
actually in the bundle.

---

## How it works

Capture a burst of frames. Mask for HUD-bright pixels rather than for a
colour, because a hostile label is red and a friendly one is cyan and one
brightness rule sees both. Group the surviving pixels into label boxes, pair
each name with the range line beneath it, and OCR only those crops. Then vote
across the burst, so a single bad read cannot name someone.

### A warning about the numbers

Every threshold in `sc_detector.py` is a measurement, not a guess. Each one was
chosen by running a 48-frame validation set and comparing what changed, and
several obvious-looking improvements were tried and rejected because they made
things worse.

**That validation set is not in this repository, and neither is the record of
those measurements.** The frames are screenshots of other players' ships, and
the write-up names roughly 150 handles encountered while testing. Publishing
either would hand out a log of who was flown near and when, which is not the
authors' to give away.

The practical consequence: if you change a threshold here, nothing in this
repository can tell you that you were wrong. Build your own corpus first.
Thresholds calibrated on one scene type break on others, and that is the single
most reliable way to make this tool worse.
