# sc-watch

Reads player handles off the Star Citizen flight HUD after a ping, checks them
against a local reputation database and RSI org data, announces them by audio,
and shows them as tiles in a local web page.

Windows only.

**[Download the latest release](../../releases/latest)**

---

## New in 0.2.1

**The Piracy and Smuggling flags no longer need starcitizen-api.com.** SENTRY
now gives each org's declared focus, so they work without an API key - and
without a service that has been failing about half the time.

Fixed:

- **"HTTP 500" errors that stayed after retrying.** They came from
  starcitizen-api.com, asked as a fallback while SENTRY was still reading an
  org. Tiles now name the service that failed, and say when SENTRY is still
  reading an org.
- **Every lookup failing for an hour at a time.** SENTRY limits how many
  players it fetches from RSI on the spot - anyone it does not have yet, which
  includes every misread. Hitting that limit used to switch SENTRY off
  entirely, so every lookup fell to starcitizen-api.com. Now only those
  fetches wait, and players SENTRY already has are answered as normal. See
  [When a lookup fails](#when-a-lookup-fails).
- **A flagged org's title stayed blank** if SENTRY had not read the org's
  focus yet when you flagged it. The name is now taken either way, and orgs
  already flagged without one are named in the background.
- **Without a key, correcting a misread in an audit** warned that both names
  were real accounts. A name nobody can check is now treated as unchecked, not
  as real.
- **Renamed handles.** Asked about a handle somebody has since renamed away
  from, SENTRY answers with that person's new profile; that answer is no longer
  filed under the old handle.

New:

- **Tiles finish themselves.** A profile waiting on an org's focus, or a
  contact whose lookup failed, is checked again in the background - every 10
  minutes at first, then less often - instead of waiting until you meet that
  player again. What it finds shows on the tile; it is not spoken.
- **Old profiles are refreshed through SENTRY**, which reads RSI again, rather
  than being sent to the other service.
- **About half the requests to SENTRY.** The same question asked twice in a
  few seconds is answered once, lookups from the page share one pace, and the
  clean-up jobs no longer spend SENTRY's hourly allowance: *Remove handles with
  no RSI account* never asks it to fetch the misreads it is looking for, and
  *Look up contacts that were never checked* does 60 a run.
- **A log of what was asked of SENTRY**, for working out lookup problems. No
  names in it - see [Where your data lives](#where-your-data-lives).

Without a key, an org you have not seen before still gets no **Syndicate**
mark - that comes from starcitizen-api.com. See
[The API key](#the-api-key-and-what-you-get-without-one).

**Upgrading from 0.2.0:** copy your `data` folder into the new one, as in
[Upgrading](#upgrading).

## New in 0.2.0

**Coming from 0.1.x? Read [Upgrading](#upgrading) first.** Your data is not
where this version keeps it, and sc-watch will look empty until you bring it
over. Nothing of yours has been deleted.

Fixed - worth knowing if 0.1.x seemed broken:

- **Spoken alerts only ever said the first line of a session.** Every alert
  after it was silent, which looked like a sound problem on your end. They are
  all spoken now.
- **Restart the watcher** closed it in the downloaded build instead of
  restarting it.
- **Audit pictures never showed** in the downloaded build, and the audit
  **dismiss** button did nothing.
- **Flagged orgs held about a quarter of their members**: member lists stopped
  after the first page.
- Two requests arriving at once could show a stranger as watch-listed.
- Two chat-reading bugs: where the chat box was looked for, and how a line's
  channel tag was split from the name.

New:

- **One zip, no installer.** Everything, your data included, lives in the
  folder you unpack. See [Install](#install).
- **[Shared watchlists](#shared-watchlists-1)**, kept apart from your own
  records - and one list comes with the tool.
- **An Orgs tab** for flagged orgs: the member list, the members who have left
  (still flagged), and members you add by hand because the public roster
  hides them. See [Flagging an org](#flagging-an-org).
- **An API key is now optional.** Lookups go through SENTRY first, which also
  shows the other orgs a player belongs to. See
  [The API key](#the-api-key-and-what-you-get-without-one).
- **[Sharing with your org](#sharing-with-your-org)**: swap reports, flagged
  orgs and hand-added members with an org mate, and remove everything of
  theirs in one click.
- **Spoken alerts you can reword or switch off**, line by line. See
  [Audio](#audio).
- Your own report reasons; filling in an org's focus or an account's age when
  a lookup could not get it; a **to audit** view; lookups that run on the tile
  instead of in a window; one-click support bundles.

Removed: the *Read ONLY the centre, every time* setting. Bind the centre-read
key instead - it does the same thing when you want it.

---

## Quick start

1. Download **sc-watch-x.y.z.zip** from the releases page and unpack it
   anywhere you can write to.
2. In Star Citizen's graphics options, **turn chromatic aberration off**. This
   is not optional. With it on the reader gets roughly one label in ten.
3. Run **sc-watch.bat**. Two windows open and a browser tab appears. In game,
   ping, then press **`0`**.

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

One download, no installer. The zip is the whole program: Python, every
dependency and the OCR models are already inside it. Nothing is written
outside the folder you unpack it into.

### Unpack and run

1. Download **sc-watch-x.y.z.zip**.
2. **Right-click the zip, choose Properties, tick Unblock**, then Apply.
   Windows marks downloaded files and will otherwise complain when you run the
   executables.
3. Extract it somewhere you can write to. `D:\Games\sc-watch` is fine.
   **Do not extract into `C:\Program Files`**, which is read-only for normal
   accounts.
4. Run **`sc-watch.bat`**. Two console windows open and a browser tab appears.

Windows may warn about an unrecognised publisher. That is because the build is
not code-signed, which costs money and a registered organisation. Choose
**More info**, then **Run anyway**.

**Everything lives in that one folder.** Your database, settings and
screenshots go in `data\` beside the executables, so the whole installation is
one directory you can move, back up, or zip and send to someone. That is what
the file named **`portable.txt`** in the folder does.

Delete `portable.txt` and your data goes to `%LOCALAPPDATA%\sc-watch` instead,
which is worth doing if you would rather unpack a new version over the old
folder without thinking about it. Either way:

```
sc-watch.exe --where
```

prints exactly where the program and your data are.

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

**From 0.1.x.** Every 0.1.x build kept your data in `%LOCALAPPDATA%\sc-watch`;
this version keeps it in the `data` folder beside the program. Until you bring
it over, sc-watch opens an empty database - and says so at the top of the page
and in the watcher window. The old data is untouched.

1. Close both sc-watch windows.
2. If you installed 0.1.x with its setup program, uninstall it (Windows
   Settings, Apps). **When it asks whether to delete your data, answer No.**
3. Unpack the new zip. In the new folder, make a folder called **`data`** if
   there is not one already.
4. Paste `%LOCALAPPDATA%\sc-watch` into Explorer's address bar, and **copy**
   everything in it into that `data` folder. Copy rather than move: the old
   folder is your backup until you are happy.
5. Run **`sc-watch.bat`**. Your contacts are back, and the note goes away.

Or skip the copy: delete `portable.txt` from the new folder and it goes on
using `%LOCALAPPDATA%\sc-watch`, as 0.1.x did.

**From 0.2.0 on.** Close both windows, unpack the new zip somewhere, and copy
your **`data`** folder into it. Nothing else in the folder is yours.

Your database is upgraded in place the first time the new version opens it.
Older databases keep working; nothing is dropped, and anything new is added
around what is already there.

If you deleted `portable.txt` and your data lives in `%LOCALAPPDATA%\sc-watch`
instead, there is nothing to copy - unpack the new zip and run it.

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
If you fly in daylight often, turn on **Haze locator** in Detection
settings, an extra pass for washed-out scenes.

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

sc-watch looks players up through two services. **SENTRY** is asked first: it
needs no key, answers in a fraction of a second for most players, lists every
org someone is in - including the side-orgs that never show on a profile - and
gives each org's declared focus. **starcitizen-api.com** is a fallback for
anything SENTRY cannot answer, and it needs a free key.

**Without a key** you still get which orgs someone is in (all of them), each
org's focus - the **Piracy** and **Smuggling** flags - whether any is one you
have flagged, how old the account is, and the check that refuses to merge
`SLIVER` and `5LIVER` because both are real accounts - all from SENTRY.

**A key adds three things:**

- **Knowing a misread is a misread.** Only starcitizen-api.com is ever trusted
  to say an account does *not* exist. SENTRY not having someone is a
  database's silence, not proof - so without a key, a handle nobody has heard
  of is shown as unchecked rather than flagged for you to correct.
- **The Syndicate mark on orgs you have not seen before.** It comes from an
  org's type, which SENTRY does not carry. sc-watch asks starcitizen-api.com
  for it when it can, and never waits on it.
- **A fallback** when SENTRY is down or does not know someone.

If SENTRY's copy of a player is out of date, sc-watch asks SENTRY to read RSI
again rather than going elsewhere: **try now** on a tile waits up to half a
minute for that, and the watcher picks up the new copy on its next try.

The choice between the two is **Settings → RSI lookups → Lookup source**:
`sentry` (the default) or `sci` for starcitizen-api.com only.

### Getting one

1. Go to **starcitizen-api.com** and register. The free tier is enough.
2. Copy your key.
3. Paste it into **Settings → RSI lookups** in the web UI and press **save
   key**. Without a key, the UI shows the same box in a notice at the top.
4. Restart the watcher - the button appears once the key is saved.

The UI saves it for you as `sc_api_key.txt` in your data folder. To do that by
hand instead: `sc-watch.exe --where` prints the folder, and the file holds just
the key on one line - no quotes, no `KEY=` prefix, and watch that Explorer does
not make it `sc_api_key.txt.txt`. An environment variable named `SC_API_KEY`
also works, and wins over the file.

Without a key, the watcher's console says so at startup, lists what you are
missing, and prints the exact path of the file for your install.

**Confirm it worked:** after the restart, the notice about a missing key is
gone from the top of the page and from the watcher window. A handle that does
not exist is then marked as having no RSI account, rather than left unchecked.

The key is stored in plain text on your machine and is sent only to
starcitizen-api.com.

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
| **Chat window region** *(adv)* | `auto` | Where chat sits. `auto` works it out from your resolution. Only set it - x, y, width, height as fractions of the screen, e.g. `0.15,0.30,0.35,0.45` - if chat reads nothing and you have moved the chat window. |
| **Wide + centre key** | `off` | Reads the **whole screen and the centre on one press** and merges both. See [Wide + centre](#wide-and-centre-the-one-to-try-if-reads-are-poor) below. Set it to a key or button, or leave `off`. |
| **Make the normal read key do both** | off | Turns every normal read into a wide + centre read, with no second binding to remember. Simpler, and you pay the extra cost on every ping. |
| **Focused box size** | `auto` | The centre region the focus key reads. `auto` sizes it from your screen height: 1200x600 at 1440 tall, 900x450 at 1080p, 1800x900 at 4K. Bigger tolerates your target drifting off centre; smaller is faster. Give pixels to override, e.g. `1600x800` if you keep just missing. |
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
| **Range gate** *(adv)* | off | An extra check that a range line really is one. Rejects more junk and occasionally a real contact with it. |
| **Re-read dim range lines** | off | **Turn this on if you can plainly see names that never get reported.** Some displays draw the range line dimmer than the name above it, and one brightness rule then keeps half a label and throws the contact away. Costs about half a second per read. This completely fixed one tester's setup. |
| **Haze locator** | off | For washed-out daylight and atmospheric flight. Turn on if you fly in atmosphere and detection falls off a cliff. |
| **Re-announce after** | `300 s` | How long before the same contact is called out again. `60` if you want reminders, higher if the same person keeps interrupting. |
| **Pairing radius** *(adv)* | `180 px` | How far below a name its range line may sit and still be paired with it. |
| **Contact expiry** *(adv)* | `20 s` | How long a contact is remembered by screen position after it stops being seen. |

Contacts the game labels `UNKNOWN` are never reported. There is no setting for
it, because there was no useful setting of it: an UNKNOWN is an asteroid, a
cow, a crate or anything else the game has not identified, it is the most
common label on a busy screen, and you can already tell what it is from your
own instruments. They are discarded the moment the label is read, before any
voting, database work or lookup happens.

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

**Spoken alerts** (its own page under Settings) lists every line the voice can
say - "Warning, NAME, 2.4 kilometres, Piracy", "Known contact, NAME, ..." - and
the pieces those are built from: one for each reason someone can be known
(your report, a shared report, a flagged org, the watch list) and one for each
org flag (Piracy, Syndicate, Smuggling, Infiltration, new account). Reword any
of them, press **hear it** to listen, or untick **on** to stop it: a flag or
reason that is off no longer announces anybody, and a contact with nothing
else switched on just gets the ordinary tone. Changes apply from the next
alert, without a restart.

Each line takes a few seconds to say, so in a busy fight alerts can arrive
faster than the voice can read them out. One still waiting 20 seconds after
its read is skipped rather than said late - it is still on the tile and in
the watcher window.

### RSI lookups

*When to look a handle up, and where. SENTRY needs no key - see
[The API key](#the-api-key-and-what-you-get-without-one).*

| setting | default | what it does |
|---|---|---|
| **Look handles up** | on | Look players up at all. Off, the watcher looks nobody up and the background checks ask nothing; buttons you press still do. |
| **Lookup source** | `sentry` | `sentry` asks SENTRY first and starcitizen-api.com for what it cannot answer. `sci` asks only starcitizen-api.com, the way it worked before SENTRY - and then nothing is looked up without a key. |
| **Votes before lookup** | `2` | How confident the reader must be before a name is looked up. Lower means more lookups on possibly-misread names. Confirming an audit by hand always looks up. |
| **API mode** *(adv)* | `auto` | For starcitizen-api.com only. `auto` lets it answer from its own stored copy where it has one; `live` makes it read RSI every time, which is slower; `cache` only ever uses its stored copies. Member lists always use `live`. Leave it on `auto`. |

### Shared watchlists

*Lists other people keep. See [Shared watchlists](#shared-watchlists-1) for
what they are and how to remove one.*

| setting | default | what it does |
|---|---|---|
| **Use shared watchlists** | on | Off silences every imported list at once. Nothing is deleted, and the panel still works so you can turn it back on. |
| **Flag lookalike spellings** *(adv)* | on | Also flag a handle that could be an OCR misreading of a listed one. These are drawn as "could match", never as a hit, and never raise an alert on their own. |

### Audit

*Keeping the picture behind a read, so you can check it by eye.*

| setting | default | what it does |
|---|---|---|
| **Save uncertain reads** | on | Saves the cropped pixels behind reads the tool is unsure about. **Leave this on.** It is how a wrong name gets corrected instead of hardening into a record. |
| **Keep audits for** | `7 days` | How long those rows and their images survive. |
| **Keep the full frame** | on | Saves the whole frame next to the crops so you can see the context. Uses more disk. |
| **Frame quality** *(adv)* | `90` | JPEG quality for those frames. Below about 80 the text gets hard to judge by eye, which defeats the point. |
| **Write a session log** | on | A plain-text log of what the watcher did, in `debug_bursts/`. It is what a support bundle is mostly made of. |

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
6. **Daylight or atmospheric flight.** Turn on **Haze locator**.
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

## Shared watchlists

Other people keep lists of players they say have done something. This reads
them, and keeps them at arm's length.

A list lives in its own database, `watchlist.db`, and **never touches your own
records**. A claim from a list cannot move your score, cannot merge two
identities, and is drawn on a tile in its own colour with the list named on it:
*listed on <the list>*. It is information about what somebody else said, for
you to do what you like with - not a verdict this tool has reached.

**One list ships with the tool**, marked *came with sc-watch* in the panel so
you can tell it from anything you import. It is other players' claims about
other players, collected from a source that checks them, with the reasons
they gave. If you would rather not have it: **Settings → Shared watchlists →
forget**, and it is gone for good - deleting a list is permanent and it does
not come back on the next launch.

**Importing one.** Paste what you were given - RSI links, bare handles, or
`handle - what they did`, one per line. Press **preview** first: it shows you
exactly what it read and what it could not, before anything is stored. A token
that is not a legal handle is never guessed at, because a mangled handle in a
griefer list is an accusation against whoever really owns that name.

**Silence** keeps a list but stops it flagging. **Forget** deletes it and
every claim it ever made, leaving nothing behind in your own records.

Some lists hedge - "some of these are still vague" - and an entry under a
caveat like that is stored as **unconfirmed** and drawn differently. A
lookalike spelling is drawn differently again, and says *could match* rather
than *listed on*: `SLIVER` and `5LIVER` are two different people.

---

## When a lookup fails

Profile lookups go through two third-party services, and some days one of
them is slow, rate limited, or down. None of that should look like "this
player is fine".

- A tile that could not be looked up says **RSI lookup failed**, with the
  reason, and offers a **try now** button. It never says "no flags".
- A failed lookup is retried automatically: twice within a minute by the
  watcher, and - for contacts you have met - again later in the background,
  at most every 6 hours.
- After three failures in a row, sc-watch stops asking that service for a
  minute, then two, up to five - so a dead service does not become a
  five-second wait on every contact. The first request after a pause is a probe; the console says when
  lookups are paused and when they resume.
- If the service cannot be reached and there is an **old stored profile**, it
  is shown rather than nothing, with its age on the tile and a refresh button.
  Orgs and account ages change slowly; a six-week-old profile is most of the
  answer, and on a bad day it is the only one.
- A tile's **look up**, **try now** or **refresh** button asks again straight
  away, skipping your cache, while you carry on: the tile says *looking up…*,
  then changes - or says *failed* beside the reason. The full answer is in the
  line at the bottom of the page. SENTRY's copy is only used for it if SENTRY
  read RSI within the last day; otherwise SENTRY is asked to read RSI again,
  for up to half a minute, and only then the other service - keeping the other
  orgs SENTRY saw. **Look up** in the header shows an answer in full,
  including which service gave it.
- **Two services, failing separately.** If SENTRY is slow or down, lookups go
  to starcitizen-api.com without waiting; if starcitizen-api.com is down,
  SENTRY still answers for anyone it has. Each pauses on its own.
- **SENTRY is rate limiting new lookups.** SENTRY fetches a player it does
  not have yet - including a misread - from RSI on the spot, and limits how
  many of those it does per hour. When it says to wait, sc-watch stops asking
  for them until the time is up, and the tile says so. Players SENTRY already
  has keep being answered meanwhile.
- **Focus unknown - SENTRY is still reading it.** The first time anyone asks
  SENTRY about an org it has not read lately, it reads RSI in the background,
  which takes a few minutes. The tile shows everything else meanwhile, marked
  *focus unknown*. The watcher checks back after 1½ and 4 minutes, then the
  page keeps checking in the background - every 10 minutes at first, then
  less often - and the tile fills in when the answer arrives. **Try now** asks
  again whenever you like.

---

## Sending a support bundle

If something goes wrong, **Settings → Storage & cleanup → Report a problem**
builds a zip you can send. It is deliberately small in what it takes:

**In it:** the session logs (what the watcher did and what it read), your
settings with secrets removed, your report reasons, and - unless you choose
*text only* - the screenshots behind recent reads.

**Not in it:** your reputation database, your imported watchlists, the profile
cache, your sightings log, and your API key. The manifest inside the zip says
all of this, so you can check before you send it.

**Read the warning about screenshots.** A full frame is your whole screen at
that moment. If a QR code, an account page, a second monitor or a chat overlay
was visible, it is in the picture. Open a few before sending, or use *text
only, no screenshots*, which sends the logs alone.

---

## Where your data lives

Everything is local. No account, no telemetry, no server.

Run `sc-watch.exe --where` to see the exact paths. Normally `data\` beside
the program, containing:

| file | what it holds |
|---|---|
| `sc-watch.db` | sightings, reports, orgs. The important one. |
| `watchlist.db` | lists other people keep, including the one that ships with the tool |
| `rsi_cache.db` | cached profile lookups, so the same handle is not fetched twice |
| `sc_api_key.txt` | your API key, if you added one |
| `settings.json` | everything from the Settings tab |
| `flags.json` | your report reasons, if you changed them |
| `audit/` | cropped screenshots of labels you asked to check by eye |
| `tones/` | the alert sounds, replaceable |
| `sightings.csv` | a plain-text log of everything seen |
| `debug_bursts/`, `live_frames/` | session logs, and captured frames if enabled. Frames are large and swept automatically. |
| `debug_bursts/sentry_requests.log` | one line per request sent to SENTRY - why, what kind, the answer - for working out lookup problems. No names in it, never sent anywhere, and not in support bundles. Kept under 4 MB. |

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
one of them the other's history is the worst mistake this tool could make.
SENTRY answers that check for anyone it has, key or no key. A name nobody can
check - no key, and SENTRY has never heard of it - is not taken as a real
account: a merge you confirm by hand then goes through, and the page tells you
it could not be checked.

### The reasons you can report under

The reasons on the flag menu are yours to change: **Settings → Report
reasons**. Each has a label, a colour, and whether it counts against someone,
for them, or neither - so "fair fight" or "helped me out" is as easy to add as
another kind of griefing. A reason you retire stays readable on the reports
already filed under it, rather than turning them into a raw key.

Flagging in the moment costs one click and no reason: press **flag** while
something is shooting at you and give it a reason later, from the tile or the
header count.

### Flagging an org

The **orgs** button in the header opens a tab of its own. Flag an org by its
SID and every member is marked the first time you meet them, with no report
written against anyone.

A flag alone reaches almost nobody, so flagging pulls the org's member list.
A profile lookup does not always show every org a player is in - the older
lookup service shows only the main one - and a piracy side-org is nearly always
an affiliate. The member list is the sure route to those people.

Pick an org and you get its roster: each member's rank, how old their account
is, which other orgs they are in - other flagged ones in red - whether you have
met them, and any reports you have filed. Click someone you have met to open
your record of them.

**Look up members** fills in the profiles the roster does not carry, one
request a second in the background, with a progress line and a stop button. It
skips anyone who already has a current profile. Nobody from a roster becomes a
contact on your grid: you have to actually meet them.

**Former members.** A refresh records who is new, who has dropped off the
roster since the last pull, and who has come back. Someone who drops off stays
flagged, because leaving is also what hiding your membership looks like - the
API leaves hidden members off the roster entirely - so the tile says
*off it since 21 Sep (left, or hid it)* rather than claiming they left.

Pulling a list is one request a second, a page at a time - 100 members a page
from SENTRY - so a large org takes a moment. It runs in the background and nothing pulls one on a timer.
If the list comes back empty or stops part way, nothing is recorded as having
left: a failed request must not turn into a claim about a person.

**Adding members by hand.** Some orgs keep most of their people off the public
roster. Put someone on a flagged org's roster yourself - from the org's page
(**add a member by hand**) or from a contact's detail window - with a note of
what you know. They carry the flag like any listed member, whether or not you
have met them yet, and **a refresh never drops someone you added**: only you
take them off. Everywhere the membership shows, it says *added by you* - or
*added by NAME*, for someone an org mate's file added. Handles must be exact;
nothing is matched to a similar-looking name.

**Filling gaps.** When a lookup could not get something - an org's focus, its
name, when an account was made - the contact's detail window lets you enter it
from their RSI page. It shows as *yours*, and RSI's own answer replaces it the
moment a lookup gets one.

This records membership, not blame. Unflagging removes the membership rows and
the history with it, after a second click that says so.

## Sharing with your org

**Settings → Sharing** swaps records with an org mate you trust.

**Export** makes a small .zip of your own records: your reports (who, which
reason, your note, when), the reasons they use, the orgs you flagged, and the
members you added to rosters by hand. You give the name to be shown with them -
your in-game name is best. It leaves out where and when you played,
screenshots, settings, your API key, watchlists, and anything you imported from
someone else - their reports, and the orgs their files flagged. Send the file
however your org shares files.

**Import** shows what a file holds before anything is added. Their reports
appear on tiles in the usual colours with a small *from NAME* note, and count
like your own - you chose to trust them. Importing a newer file from the same
person updates what you have from them rather than doubling it, and drops what
they have since deleted. Your own records are never touched. **Remove
everything from NAME** takes it all back out in one click; orgs their import
flagged stay flagged until you unflag them.

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
