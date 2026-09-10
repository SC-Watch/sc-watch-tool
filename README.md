# sc-watch

Reads player handles off the Star Citizen flight HUD after a ping, checks them
against a local reputation database and RSI org data, announces them by audio,
and shows them as tiles in a local web page.

Windows only.

**[Download the latest release](../../releases/latest)**

---

## Quick start

1. Download **sc-watch-x.y.z-setup.exe** from the releases page and run it.
2. In Star Citizen's graphics options, **turn chromatic aberration off**. This
   is not optional. With it on the reader gets roughly one label in ten.
3. Launch sc-watch from the Start Menu. Two windows open and a browser tab
   appears. In game, ping, then press **`0`**.

Contacts appear as tiles within a second or two and are read out loud.

If nothing appears, skip to [When it reads nothing](#when-it-reads-nothing).
Nine times out of ten it is chromatic aberration.

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

Overlays that inject into the render pipeline and tools that synthesise input
are what anti-cheat systems care about. This does neither. It is a screen
reader that happens to be pointed at a game.

Not affiliated with or endorsed by Cloud Imperium Games.

---

## Install

### The installer

Download **sc-watch-x.y.z-setup.exe** and run it. No Python, no dependencies,
no administrator prompt.

Windows SmartScreen will warn about an unrecognised publisher. That is because
the build is not code-signed, which costs money and a registered organisation.
Click **More info**, then **Run anyway**. If you would rather not, use the zip.

**Where it puts things.** Two separate places, on purpose:

| what | where |
|---|---|
| the program | `%LOCALAPPDATA%\Programs\sc-watch` |
| your data | `%LOCALAPPDATA%\sc-watch` |
| Start Menu | `sc-watch`, with a UI-only shortcut and a "Where is my data" shortcut |

Paste `%LOCALAPPDATA%` into an Explorer address bar to get there.

They are separate so that upgrading or uninstalling cannot take your database
with it. Uninstalling removes the program and then asks, separately, whether to
delete your data. It defaults to **no**.

If you tick the option to install for all users, the program goes to
`C:\Program Files\sc-watch` instead. Your data does not move.

### The portable zip

Use this if you dislike installers, want it on a USB stick, or want it in a
folder you back up yourself.

1. Download **sc-watch-x.y.z.zip**.
2. **Right-click the zip, choose Properties, and tick Unblock**, then Apply.
   Windows marks downloaded files and will otherwise complain when you run the
   executables.
3. Extract it somewhere you can write to. `D:\Games\sc-watch` is fine.
   **Do not extract into `C:\Program Files`**, which is read-only for normal
   accounts.
4. Run **`sc-watch.bat`** in the extracted folder. Two console windows open.

By default the portable copy still keeps your data in `%LOCALAPPDATA%\sc-watch`
like the installed one. To keep everything together in the folder instead,
create an empty file called **`portable.txt`** next to `sc-watch.exe`. Data
then lives in a `data` sub-folder beside the program, and the whole thing moves
as one directory.

To make one: open the folder, right-click, New, Text Document, and name it
`portable.txt`. Make sure Windows has not called it `portable.txt.txt`. Turn on
File name extensions in Explorer's View menu if you are unsure.

Confirm which mode you are in at any time:

```
sc-watch.exe --where
```

### From source

For anyone who wants to read or change the code. Python 3.11 or newer.

```
pip install -r requirements.txt
python watch.py
```

Run the UI in a second terminal with `python ui_server.py`, or use
`sc-watch.bat` to start both.

Run from source and everything stays beside the scripts, exactly where you
would expect. The pinned versions matter, see
[A warning about the numbers](#a-warning-about-the-numbers).

### Upgrading

Run the new installer over the top. It closes any running copy first. Your
database, settings and audit screenshots are untouched because they live
somewhere else.

For the zip, extract the new one over the old folder.

---

## Before it will read anything

Four things decide whether this works, and three of them are on your screen
rather than in this tool.

### 1. Turn chromatic aberration OFF

Star Citizen, Options, Graphics. This one is close to non-negotiable.

Chromatic aberration splits every bright edge into red and blue fringes. HUD
text is thin and bright, so the effect lands squarely on the glyphs and the
recogniser stops committing to them. With it on, expect roughly **one label in
ten**. With it off, expect nearly all of them.

If you change one thing, change this.

### 2. Turn the debug overlay off

If you have ever typed `r_displayInfo 2` in the console, set it back:

```
r_displayInfo 0
```

The overlay is bright text in the corner of the screen. It is not fatal, but it
eats about **a third of the per-frame reading budget** on text that is not a
contact, and that budget is what decides whether the ship you are actually
pointing at gets read before the burst ends.

### 3. Do not let other HUD elements sit on top of names

This is the one people do not think of.

The reader finds a contact by locating a bright **name** and pairing it with
the bright **range line directly beneath it**, centred within a few pixels. It
then reads only those two small crops.

When another bright thing overlaps that pair, three things go wrong:

- **The pair merges.** A name sitting over a fuel gauge or a mission marker
  merges into one blob during segmentation, and the blob is the wrong shape, so
  it is discarded. A real case: a handle overlapping the H-FUEL readout came
  back as `2.0kga`, which is neither the name nor the range.
- **The range line is captured by the wrong thing.** If a brighter horizontal
  element sits where the range should be, the name pairs with that instead and
  the contact is thrown out for having an implausible range.
- **The name is truncated.** A cockpit strut or a bracket crossing the first
  letter loses it. `KEPSS` becomes `EPSS`, which is then a different person as
  far as any database is concerned.

**What to do:** if a contact is not being read, yaw a few degrees so its label
sits over empty space, and press the key again. Contacts against open sky or
dark space read best. Labels near the middle of the screen, over the cockpit
frame or over the MFDs, are the worst case.

The centre-read key (**`9`** by default) helps here for a different reason: it
reads only the middle of the screen, so it spends its whole budget on whatever
you are pointing at rather than competing with twenty other labels.

### 4. Watch out for bright backgrounds

The reader finds text by **brightness**, not by colour. That choice is
deliberate, because a hostile label is red and a friendly one is cyan and one
brightness rule sees both. The cost is that it needs the label to be brighter
than what is behind it.

Against a bright background the label stops standing out. Measured on a real
frame of a contact in front of a sunlit planet: the label peaked just below the
threshold and **not one pixel of it** registered, so nothing was found at all.
The same handle against dark space reads perfectly.

Worst cases, in rough order:

- Flying toward the sun, or with the sun just off-screen creating glare
- Atmospheric flight in daylight, especially in haze
- A bright planet surface filling the view
- Station interiors with lots of white surfaces

**What to do:** put the contact against sky or space if you can, and try again.
If you fly in daylight often, turn on **Haze mode** in Detection settings,
which is tuned for washed-out scenes.

### The limitation this creates, stated plainly

This tool cannot see a label that does not stand out from its background, and
lowering the threshold to catch dim labels does not work. That has been
measured: dropping the brightness floor lost four real detections and removed
no junk, because a lower floor merges nearby glyphs into blobs that the size
filter then throws away.

So there will always be contacts it misses. **Missing a contact is the failure
mode this tool is built to accept.** The alternative, a lower bar that invents
names, is far worse, because an invented name attached to a reputation is a
claim about a real person that nobody made.

If it misses one, ping again from a slightly different angle.

---

## The API key, and what you get without one

sc-watch works with no key. You lose two things.

**Without a key** you get handle reading, sightings, your own reports, audio
alerts and the web UI. You do not get org membership, account age, or the
check that stops two similar-looking handles being merged into one person.

**With a key** you also get: which org someone flies for, whether that org is
one you have flagged, how long the account has existed, and a verification step
that refuses to merge `SLIVER` and `5LIVER` because both are real accounts.

### Getting one

1. Go to **starcitizen-api.com** and register. The free tier is enough.
2. Copy your key.
3. Find your data folder. The easy way is the **Where is my data** shortcut in
   the Start Menu, or run `sc-watch.exe --where`. It is normally
   `%LOCALAPPDATA%\sc-watch`.
4. Create a file there called **`sc_api_key.txt`** containing just the key, on
   one line, with nothing else. No quotes, no `KEY=` prefix.
5. Restart the watcher.

Check File name extensions in Explorer so you do not end up with
`sc_api_key.txt.txt`, which will not be found.

As an alternative, set an environment variable named `SC_API_KEY` instead. The
file is easier.

**Confirm it worked:** open a contact tile in the UI. With a working key you
see org information or an explicit "no org". Without one, tiles say the profile
was never looked up.

The key is stored in plain text on your machine, is never sent anywhere except
starcitizen-api.com, and never leaves your PC otherwise.

---

## Using it day to day

Ping, then press the read key. That is the loop.

| key | what it does |
|---|---|
| `0` | read the whole screen |
| `9` | read only the centre, for whoever you are pointing at |
| off | read the chat window and list who is talking |

All three are rebindable, including to joystick buttons and combinations.

**The web UI** is at `http://127.0.0.1:8731`. Each contact is a tile. Click one
to open its window, where you can flag them, add a reason, correct a misread,
mark it as not-a-player, or look up the profile.

**Amber digits** in a handle mean two readings disagreed on that character and
the tool has not yet confirmed which is right. Once enough sightings agree, or
you confirm it by eye, the tinting stops.

**The audit queue**, counted in the header, is reads the tool is unsure about.
Opening one shows you the actual cropped pixels it read, so you can say what
the label really said. This is the single most useful thing you can do for
accuracy.

---

## Settings reference

Everything below is in the **Settings** tab of the web UI, and the names here
match the labels you see on screen exactly. Advanced settings are hidden until
you tick **Show advanced settings**, and they are hidden because the defaults
were measured and are usually right.

Changing most Capture and Detection settings needs the watcher restarted.
There is a **Restart watcher** button in Maintenance.

### Capture

*What starts a read, and how much of the screen it looks at.*

| setting | default | what it does |
|---|---|---|
| **Trigger** | `key` | `key` reads when you press the key. `auto` reads continuously on a timer. Leave it on `key`: continuous reading burns CPU and re-reads the same contacts. |
| **Read key** | `0` | Full-screen read. Pick something you never touch in flight. Good choices: `0`, `-`, `=`, or a spare joystick button. |
| **Focused read key** | `9` | Centre-only read. The best key on the tool, because it spends the whole reading budget on whatever you are pointing at instead of competing with twenty other labels. |
| **Chat read key** | `off` | Reads the chat window and lists who is talking. Costs no ping and finds people you will never get a HUD label for. Try `8` if you spend time in busy areas. |
| **Chat window region** *(adv)* | `0.15,0.30,0.35,0.45` | Where chat sits, as fractions of the screen: x, y, width, height. Only touch this if chat reads nothing and you have moved the window. |
| **Wide + centre key** | `off` | Reads the **whole screen and the centre on one press** and merges both. See [Wide + centre](#wide-and-centre-the-one-to-try-if-reads-are-poor) below. Set it to a key or button, or leave `off`. |
| **Make the normal read key do both** | off | Turns every normal read into a wide + centre read, with no second binding to remember. Simpler, and you pay the extra cost on every ping. |
| **Focused box size** | `1200x600` | The centre region the focus key reads, in pixels. Bigger tolerates your target drifting off centre; smaller is faster. Try `1600x800` if you keep just missing, `900x500` if you want speed. |
| **Read ONLY the centre, every time** *(adv)* | off | Replaces the normal full-screen read with a centre-only one. **It does not do both.** Contacts outside the box stop being seen at all. Leave it off; the focus key already gives you centre reads on demand. |
| **Mask focused reads** *(adv)* | off | Changes how centre reads are prepared for the recogniser. Slower, and only occasionally better. Leave off. |
| **Frames per read** | `3` | How many frames one press captures. Frames vote against each other, so a single bad read cannot name someone. `3` is the sweet spot. `5` for difficult scenes. `1` only for testing. |
| **Gap between frames** | `0.1 s` | How long to wait between those frames. Tight is better: contacts drift and labels fade, and a late frame often catches neither. **Start at `0.1`.** Above about `0.5` the hit rate drops off, so treat `0.5` as the practical ceiling rather than the range end. |
| **Delay before first frame** | `0 s` | Waits after the key press before grabbing. Set `0.3` to `0.5` if you tend to press the key while the scan animation is still sweeping. Every second here is a second before you get an answer. |
| **Monitor** | `1` | Which screen the game is on, counting from 1. If you get black frames or the wrong screen, this is the setting. |
| **Key poll rate** *(adv)* | `5 Hz` | How often the trigger key is checked. `10` if presses feel missed. Costs a little CPU. |
| **Queued reads** *(adv)* | `3` | How many presses can stack up while a read is already running. Raise it if you mash the key in a busy moment. |

### Detection

*How hard the reader tries, and what it is willing to believe.*

| setting | default | what it does |
|---|---|---|
| **Votes to announce** | `1` | How many frames of a burst must agree before a name is announced. `1` is fast and occasionally wrong. **`2` is the single setting most worth changing**, and costs you nothing but a moment. |

Contacts the game labels `UNKNOWN` are never reported. There is no setting for
it, because there was no useful setting of it: an UNKNOWN is an asteroid, a
cow, a crate or anything else the game has not identified, it is the most
common label on a busy screen, and you can already tell what it is from your
own instruments. They are discarded the moment the label is read, before any
voting, database work or lookup happens.

| **Range gate** *(adv)* | off | An extra check that a range line really is one. Rejects more junk and occasionally a real contact with it. |
| **Re-read dim range lines** | off | **Turn this on if you can plainly see names that never get reported.** Some displays draw the range line dimmer than the name above it, and one brightness rule then keeps half a label and throws the contact away. Costs about half a second per read. This completely fixed one tester's setup. |
| **Haze locator** | off | For washed-out daylight and atmospheric flight. Turn on if you fly in atmosphere and detection falls off a cliff. |
| **Re-announce after** | `300 s` | How long before the same contact is called out again. `60` if you want reminders, higher if the same person keeps interrupting. |
| **Pairing radius** *(adv)* | `180 px` | How far a contact may move between frames and still count as the same one. |
| **Contact expiry** *(adv)* | `20 s` | How long a contact is remembered by screen position after it stops being seen. |

### Audio

*What you hear when something is found.*

| setting | default | what it does |
|---|---|---|
| **Audio** | `both` | `off`, `tones`, `speech`, or `both`. Use `tones` if speech steps on your comms. |
| **Volume** | `0.9` | Speech volume, 0 to 1. |
| **Speech rate** | `175` | Words per minute. `140` is slower and clearer, `220` gets out of the way faster. |
| **Say handles aloud** | on | Off gives you only the tone that says "something was found". Worth turning off if the speech engine mangles the odd spellings people use, which it does. |
| **Handles spoken per read** | `2` | Stops a busy scene becoming a thirty-second monologue. |
| **Summarise above** | `3` | Past this many contacts, say "five contacts" instead of listing them. |

The tones are ordinary WAV files in your data folder. Replace them with your
own if you want different sounds.

### RSI lookups

*When to ask the API who a handle belongs to. Needs an API key.*

| setting | default | what it does |
|---|---|---|
| **Look handles up** | on | Look up profiles at all. Does nothing without a key. |
| **Votes before lookup** | `2` | How confident the reader must be before spending an API call on a name. Lower means more lookups on possibly-misread names. |
| **API mode** *(adv)* | `auto` | `auto` uses the cache and falls back to live. `cache` never touches the network. `live` always does. Leave it on `auto`. |

### Audit

*Keeping the picture behind a read, so you can check it by eye.*

| setting | default | what it does |
|---|---|---|
| **Save uncertain reads** | on | Saves the cropped pixels behind reads the tool is unsure about. **Leave this on.** It is how a wrong name gets corrected instead of hardening into a record. |
| **Keep audits for** | `7 days` | How long those rows and their images survive. |
| **Keep the full frame** | on | Saves the whole frame next to the crops so you can see the context. Uses more disk. |
| **Frame quality** *(adv)* | `90` | JPEG quality for those frames. Below about 80 the text gets hard to judge by eye, which defeats the point. |

### Storage & cleanup

*Captured frames are big. This is what stops them piling up.* During
development one directory reached 1.1 GB.

| setting | default | what it does |
|---|---|---|
| **Save every frame (debug)** | off | Writes every frame of every burst to disk. Turn on to diagnose something, then turn it back off. |
| **Delete old frames** | on | The sweep itself. Leave on. |
| **Clean up at startup** | on | Sweep when the watcher starts. |
| **Clean up every** | `30 min` | How often to sweep during a session. `0` disables the timer. |
| **Delete frames older than** | `24 h` | Age limit for captured frames. |
| **Always keep newest** | `20` | Never delete the most recent N in a directory, whatever the age rule says. |
| **Hard file cap** *(adv)* | `0` | A cap regardless of age. `0` means no cap. |

Maintenance also holds one-off jobs. Every one shows you what it would do
before it does it.

### Interface

*The web UI itself.*

| setting | default | what it does |
|---|---|---|
| **Port** | `8731` | Change if something else already uses it. Needs the UI restarted. |
| **Open a browser on start** | on | |
| **Contacts shown** | `120` | How many tiles the page renders. Lower it if the page feels heavy. |
| **Tile size** | `380 px` | Bigger tiles, fewer per row. |
| **Square tiles** | on | |
| **Show advanced settings** | off | Reveals everything marked *(adv)* above. |

### If you change one thing

**Votes to announce, from `1` to `2`.** It is the difference between a tool
that is usually right and one you can act on.

---

## Wide and centre: the one to try if reads are poor

The reader has a fixed budget of recognition work per frame. A **wide** read
covers the whole screen but splits that budget across every label on it, so a
small, dim or awkward one loses to a bright easy one. A **centre** read crops
to the middle and lifts the cap, so whatever you are pointing at gets the whole
budget to itself.

**Wide and centre does both on one press and merges the results.** Where they
overlap, the centre reading wins, because it is the better-evidenced one.

Turn it on either way:

- Set **Wide + centre key** to its own key or button, and use it when it
  matters.
- Or tick **Make the normal read key do both**, and never think about it again.

### What it costs and what it buys

Measured across nine frames from three different machines:

| | wide alone | wide + centre |
|---|---|---|
| time per frame | 0.86 s average | 1.22 s average |
| overall | 1x | **1.4x** |

Not double, because the centre crop is smaller and usually holds fewer labels.

What it buys depends entirely on how well the wide read already works on your
display. On a machine where wide reads were already good, it added nothing and
lost nothing. On a machine where they were struggling, it was the difference
between almost nothing and almost everything:

| frame | wide alone | wide + centre | recovered |
|---|---|---|---|
| 1 | 0 | 1 | `CHUTESX` |
| 2 | 0 | 0 | |
| 3 | 0 | 2 | `GMONEY7879`, `MISTI_IS` |
| 4 | 1 | 3 | `OMEGAQNYX737`, `SH-U6REST` |
| 5 | 0 | 1 | `FAIREFACEE` |

One contact across five frames became seven.

The reason it helps so much there is that the centre read does not use the
brightness locator at all. It hands the crop straight to the recogniser. So a
label that is too dim or too washed-out for the locator to find, which is the
main limitation described in
[Watch out for bright backgrounds](#4-watch-out-for-bright-backgrounds), can
still be read this way. One of those recovered contacts was a label with
literally **one pixel** above the brightness threshold.

**If you are missing contacts you can plainly see, try this.** It costs a
third more time per read.

### The catch, measured

It is not free, and the cost is not just time.

The centre read skips the brightness locator, which is what lets it see dim
labels. It also means it will read text the locator would have rejected. In a
**crowded scene**, where a dozen player labels overlap each other, it finds
real people the wide read gave up on entirely, and it gets some of their names
wrong. Measured on one frame of a large group at 600 to 900 metres:

| what it reported | what the label actually said |
|---|---|
| `AAKOBOLDNAMEDKITHRAX` | correct |
| `IDRAGUA8NAME560` | `IDKAGOODNAME560` |
| `LHEMOLRYONE` | `THEMOLDYONE` |
| `6NOPSAIKHO` | `NOTSAIKHO` |
| `SAGROFASUBIRARO48` | two overlapping labels merged into one |

One of five exactly right, three close but wrong, one nonsense. Every one of
them is a real person whose label was physically overlapped by another.

**Voting does not save you here.** Reading the same frame three times gives
byte-identical results, wrong names included. Frame voting defends against
random errors; overlapping labels produce the same wrong answer every time.

What does defend you is the profile lookup: a garbled name does not resolve to
a real RSI account, and the tool says so on the tile rather than treating it as
a person. **So an API key matters more when you use this**, and so does
checking the audit queue.

### In short

| | wide only | wide and centre |
|---|---|---|
| contacts found on a difficult display | few | many |
| contacts found in a crowded scene | almost none | many |
| name accuracy in a crowded scene | — | mixed, see above |
| time per read | 1x | 1.4x |

Turn it on if you are missing people. Keep an API key configured so misreads
get caught, and glance at the audit queue afterwards.

Its other limit is the focus box: contacts outside the centre region are found
only by the wide half of the read, exactly as they are now, so nothing gets
worse. Widen **Focused box size** if your targets sit off-centre.

---

## Keys, joysticks and combinations

Press **detect** next to any key setting, then press what you want to use. It
waits for you to finish before committing, so combinations work: hold your
modifier, press the button, and it records the whole thing.

Examples of what a binding can look like:

```
0                        a keyboard key
rctrl                    a modifier on its own
joy0.b24                 button 24 on the first joystick
rctrl+joy0.b24           hold the modifier, press the button
vid231d:0127.b12         a specific device by hardware id
```

Binding by hardware id is worth using when you have several sticks, because
device order can change between reboots and a hardware id will not.

### Joystick support, and its one real limit

Joysticks are read through the legacy Windows joystick API. It installs
nothing and intercepts nothing, which is why it was chosen.

It has a hard limit of **32 buttons and 6 axes per device**, and **16 devices**
total. That limit is in the Windows API, not in this tool.

**VIRPIL devices work.** Tested against a four-device VIRPIL setup, all four
were detected and readable:

```
joy0  vid231d:0127   32 buttons, 5 axes
joy1  vid231d:0126   32 buttons, 5 axes
joy2  vid231d:011f    2 buttons, 3 axes
joy3  vid231d:3201   32 buttons, 6 axes
```

The catch is that VIRPIL boards can expose more than 32 buttons, and anything
past the 32nd is invisible here. With several devices you still have plenty of
addressable buttons, and if the button you want is out of range there is a
clean workaround: **map it to a keyboard key in VIRPIL's own software** and
bind that key instead. That is a normal thing to do with VIRPIL configuration
software and this tool cannot tell the difference.

VKB, Thrustmaster and Logitech devices work on the same terms.

To see what your machine exposes:

```
sc-watch.exe --list-inputs
```

That prints every device the API can see, with its hardware id and its button
and axis counts, which is what you need to write a binding by hand.

---

## When it reads nothing

Work down this list. It is ordered by how often each one is the answer.

1. **Chromatic aberration is on.** Turn it off. This is the answer most of the
   time.
2. **The wrong monitor.** Set **Monitor** in Capture settings.
3. **Names covered by other HUD elements.** See
   [point 3 above](#3-do-not-let-other-hud-elements-sit-on-top-of-names). Yaw
   slightly and try again.
4. **A bright background.** See
   [point 4](#4-watch-out-for-bright-backgrounds). Try the same contact against
   dark sky.
5. **Names visible but never reported.** Two things to try, in this order:
   turn on **Wide and centre** (see
   [above](#wide-and-centre-the-one-to-try-if-reads-are-poor)), which recovered
   most of a tester's missing contacts, then **Re-read dim range lines** in
   Detection.
6. **Daylight or atmospheric flight.** Turn on **Haze mode**.
7. **The debug overlay is on.** `r_displayInfo 0`.

Still stuck? Take a screenshot of your HUD with a contact visible in it, then
run this from the folder sc-watch is installed in:

```
sc-watch.exe --check-resolution "C:\path\to\myscreenshot.png"
```

It reports, in the order things break: whether the image is letterboxed,
whether any HUD text is bright enough to find, how tall your labels are in
pixels, and what the reader made of the frame. That output is the useful thing
to attach to a bug report.

To open a prompt in the right folder, type `cmd` into the Explorer address bar
while you are in the install directory and press Enter.

**Save screenshots as PNG, not JPEG.** JPEG compression damages exactly the
thin bright text this depends on, and it makes it impossible to tell whether a
problem is your HUD or the file format.

---

## Where your data lives

Everything is local. No account, no telemetry, no server.

Run `sc-watch.exe --where`, or use the Start Menu shortcut, to see the exact
paths. Normally `%LOCALAPPDATA%\sc-watch`, containing:

| file | what it holds |
|---|---|
| `sc-watch.db` | sightings, reports, orgs. The important one. |
| `rsi_cache.db` | cached profile lookups, so the same handle is not fetched twice |
| `sc_api_key.txt` | your API key, if you added one |
| `settings.json` | everything from the Settings tab |
| `audit/` | cropped screenshots of labels you asked to check by eye |
| `tones/` | the alert sounds, replaceable |
| `sightings.csv` | a plain-text log of everything seen |
| `debug_bursts/`, `live_frames/` | captured frames, if enabled. Large, swept automatically. |

**To back up, copy `sc-watch.db`.** That is your reputation history. Everything
else regenerates.

The web UI binds to `127.0.0.1` and refuses any request that does not arrive
with a loopback host header, so no other machine can reach it and no website
open in your browser can drive it.

---

## Reporting someone

A report is a claim about something a person did: a category, a note and a
date. The tool keeps that separate from facts like which org someone belongs
to, and will not invent one from the other.

It will also refuse to fold two spellings together when both resolve to real
RSI accounts, because `SLIVER` and `5LIVER` are two different people and giving
one of them the other's history is the worst mistake this tool could make. That
check needs an API key. Without one, a merge you confirm by hand goes through
unverified and the UI tells you so.

### Flagging an org

Settings has a **Flagged orgs** panel. Flag an org by its SID and every member
is marked the first time you meet them, with no report written against anyone.

A flag alone reaches almost nobody, so the panel pulls the org's member list
when you flag it. The profile lookup returns only a player's main org, and a
piracy side-org is nearly always an affiliate, which never shows up there. The
member list is the only route to those people.

Each row tells you how many members it has, when the list was last pulled, and
how many of your own contacts it actually reaches. Pulling a list is one
request per second and up to twenty pages, so a large org takes a while. It
runs in the background and nothing pulls one on a timer.

This records membership, not blame. Unflagging removes the membership rows it
added.

---

## Licence

Free software under the **GNU General Public License v3.0**. Full text in
[LICENSE](LICENSE).

Use it, study it, change it, share it. If you distribute a modified version you
must publish your source under the same terms, so nobody can take this closed
and sell it.

The licence deliberately does not restrict **use**. No open-source licence
does. The limits on misuse are in the design instead: it refuses to merge two
handles that both resolve to real accounts, it keeps org membership separate
from any claim about what a person did, and it keeps the pixels behind an
uncertain read so a bad one can be corrected rather than hardening into a
record.

No warranty. This tool reads blurry text off a screen and is sometimes wrong
about a name. Treat what it tells you accordingly.

Third-party components keep their own licences, all permissive and all
compatible with GPL-3.0, listed with full texts in `THIRD-PARTY-NOTICES.txt`.

---

## How it works

Capture a burst of frames. Mask for HUD-bright pixels rather than for a colour,
because a hostile label is red and a friendly one is cyan and one brightness
rule sees both. Group the surviving pixels into label boxes, pair each name
with the range line beneath it, and read only those crops. Then vote across the
burst, so a single bad read cannot name someone.

### A warning about the numbers

Every threshold in `sc_detector.py` is a measurement, not a guess. Each was
chosen by running a 48-frame validation set and comparing what changed, and
several obvious-looking improvements were tried and rejected because they made
things worse.

**That validation set is not in this repository, and neither is the record of
those measurements.** The frames are screenshots of other players' ships, and
the write-up names roughly 150 handles encountered while testing. Publishing
either would hand out a log of who was flown near and when, which is not ours
to give away.

The practical consequence: if you change a threshold here, nothing in this
repository can tell you that you were wrong. Build your own corpus first.
Thresholds calibrated on one scene type break on others, and that is the single
most reliable way to make this tool worse.

The dependency versions in `requirements.txt` are part of the same story. OCR
output is not stable across recogniser versions, so a different rapidocr moves
the thresholds without anyone touching them.
