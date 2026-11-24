# Overdeck as the booth's host

Written 2026-11-24 from a conversation with Dan about what Overdeck is for.
A framing and a plan, not built: what is built is marked as such, with its
commit.

## 1. What Overdeck is

The first idea was "Mixxx with our plugins": DJ software with a library,
two decks, a mixer and effects. That puts Overdeck beside Mixxx, rekordbox,
Serato and Traktor, judged on their terms (library, effects, controller
support), where it would always be catching up, and a DJ hears "has
effects", not "is different".

What Overdeck has that none of them do, together:

- **It is on Pro DJ Link both ways.** A slot can mirror a real player
  (`follow = N`, freedj's FOLLOW), and its decks are players the others see.
- **It hosts decks as plugins.** freedj decks, and any CLAP instrument: the
  sampler deck, stems, a unit.
- **It owns every bus.** Decks, the mixer plugin and, next, the real
  players' audio on inputs (ARCHITECTURE.md §7) all pass through it.
- **It owns the hardware.** Surfaces, screens and lamps over ODP.

So it is the one place that knows everything happening in the booth: every
deck's track, position, grid and phrases, real or virtual, and the audio.

**The framing: the booth's host.** The layer between the decks (real and
virtual) and the PA. The mixer is one plugin in it, not what it is.

| Framing | What it is | Risk |
|---|---|---|
| Mixer | DJ mixer software with plugin effects | The crowded category |
| Visualiser | One screen of every deck in the booth | A feature, not a product |
| Performance host | An instrument rack for DJs | Strong, a big lift |
| **Booth host** | Real CDJs and virtual decks in one synced set; mixing, effects, stems, recording and visuals from what it knows | Sprawl: it needs one use case that proves it |

**The use case that proves it:** a virtual deck beside real CDJs. A DJ with
two CDJs plugs in a laptop or a Pi running Overdeck and has a third deck (a
freedj deck, or the sampler) synced on Link and mixed in. Most of it exists.

Open: who uses it first. A club DJ with CDJs makes it the booth host; a
laptop-only DJ pulls it back toward "Mixxx"; a performer building sets
makes it the instrument host.

## 2. Stems for every deck in the booth

The feature that fits the framing. Three ways in, by what the deck can do.

### 2.1 Decks that take stems: serve them

- OpenDeck to OpenDeck: built. stemd serves a track's split stems at the
  playhead; another OpenDeck plays them about 2.4 s after a load
  (freedj-3000, `docs/design/` and memory "stemd server").
- To CDJ-3000s with OverCue (CDJMODS banks over LINK): planned
  (freedj-3000 `f4a8d5b`); OverCue still refuses our banks (emulator
  retest `78cf93a`). Finishing this is the work.

### 2.2 Stock decks: a premixed file (fixed mixes only)

Overdeck serves a stock deck a WAV whose bytes it renders on request: the
stems summed at levels Overdeck sets. The deck plays it as any file.

The catch is **when** the mix is decided: when the deck *reads* the bytes,
not when they are heard. A player reads ahead of its playhead, perhaps the
whole track (players read 32 KB at a time; how far ahead each model
buffers is not measured). A mute would reach the PA only after everything
already read; with a whole-track read, never. So this serves **chosen
versions** ("instrumental", "acapella", "no drums"), not live faders.

To work out, if built: a stable length and header for the served WAV (the
deck's waveform and analysis come from the original); an NFS file per
version (`Contents/…/x.instrumental.wav`) or one file re-rendered per
load; how the DJ picks the version (Overdeck's screen, before LOAD).

### 2.3 Live stems, the plan: the CDJ as a controller

Decided 2026-11-24 (Dan: "does it make more sense to just control freedj
in Overdeck with HID mode?"). For live stems, freedj plays and the CDJ
controls it: the CDJ in its control mode (MIDI or HID, over USB, or over
LAN on a CDJ-3000) is an Overdeck surface, and a freedj deck makes the
sound. There is nothing to lock: the jog and keys drive the deck that plays,
so stems, effects and the sampler are native, not bolted on under a
mirrored playhead.

| | Follow (2.4) | Control mode |
|---|---|---|
| Who plays | the CDJ; Overdeck chases it | freedj; the CDJ is a controller |
| Sync | a mirror on beat/position packets, weakest on jog, scratch, loops | none needed |
| Stems, effects, sampler | under a mirrored playhead | native |
| Tracks from | the DJ's stick, over LINK | Overdeck's library (or the stick read over LINK, to add) |
| Setup | none: on Link, it works | each CDJ switched to control mode and cabled to Overdeck |
| The CDJ's screen | its own | what its control mode shows, often little |

So the two serve different booths:

- **Your rig** (your decks, Overdeck on a Pi or a laptop): control mode.
  Live stems, effects and the sampler, with the CDJ as jog and keys.
- **A booth you don't control:** follow. No setup, the DJ's stick as
  usual; Overdeck knows every deck (visuals, recording, phrase sync) and can
  offer the fixed versions of 2.2.

What exists to build on:
- freedj-3000 `crates/link`: the CDJ-over-LAN HID transport (lanhid), its
  handshake secret kept out of the repo (gitignored config).
- From cdj3k-emu: MIDI is live without the handshake; the full HID control
  report (0x20) is locked behind it.
- freedj-3000 `crates/app/src/surface/`: controllers.toml maps USB MIDI to
  deck actions (DJ2Go is a preset).
- Overdeck's surface layer (ARCHITECTURE.md §1-2): hardware to ODP.

So the first driver is a mapping more than a new stack: a "CDJ as
controller" surface in Overdeck, from the CDJ's reports to ODP for one
freedj deck, and lamps back.

First step: on Dan's XDJ-1000MK2, see what its control mode sends over
USB (MIDI, HID, or both) and whether any of it needs the handshake. That
decides whether the first driver is MIDI or HID. Then: jog (touch and
turn), PLAY, CUE, hot cues, tempo, in the shape freedj's controller
mapping already has.

### 2.4 Stock decks, live, the fallback: the shadow deck

When the decks can't be switched to control mode (a booth you don't
control) and live stems are still wanted. Live stems on any deck, using
FOLLOW:

1. A slot follows the real player (`follow = N`): it loads the same track
   over LINK and holds the player's playhead.
2. Overdeck plays that track's stems itself, locked to the mirrored
   playhead, and mixes them live (faders, mutes, the stem EQ).
3. The player becomes a controller and a screen. Its sound is not used:
   Overdeck's output for that channel goes to the mixer in its place.

What it needs, and what is known:

- **Lock.** FOLLOW held an emulated CDJ-2000 (freedj-esp's firmware) within
  ±1.2 ms of its beat packets, and paused within 1.2 ms of the deck's own
  position (2026-11-24, freedj-3000 docs/FEATURES.md). A CDJ-3000 sends
  its playhead in ms (median 0, p95 7 ms against an emulated one). Good
  enough for steady playback.
- **The hard parts.** Scratching and jogging: FOLLOW is weakest there
  (beat packets stop; a CDJ-3000's position packets help). Loops, hot cue
  jumps and reverse must land in the same audio block. A slip between the
  player's own stems-off sound and ours would be heard as phasing, which
  is why the player's sound must not reach the PA at all.
- **Routing.** The player's channel on the mixer gets Overdeck's output,
  not the player's. On a booth mixer that is a cable change; with
  Overdeck's own mixer plugin it is internal.
- **CPU.** Splitting is the heavy part: HTDemucs on a Pi 5 splits a chunk
  in 6.0 s, real time when cooled, 0.8× for a whole track without cooling
  (freedj-3000 docs/FEATURES.md). Prepared stems (OverCue Desktop's, or a
  split done at load) avoid it.

The first step to prove it: one CDJ-3000 in cdj3k-emu, a follow slot, and
the slot's stems played under the player's playhead, graded the way FOLLOW
was graded (per beat, and at pause).

### 2.5 Streaming services: not this way

Dan asked about real-time stems from Spotify. As of this writing:

- Spotify came back to DJ software in September 2025 as a licensed
  integration in desktop rekordbox, Serato and djay, later on mobile.
- Even there, stems do not work on Spotify tracks, and tracks cannot be
  stored offline.

Sources: [TechCrunch, 2025-09-24](https://techcrunch.com/2025/09/24/spotify-now-integrates-directly-with-dj-software-from-rekordbox-serato-and-djay),
[Spotify newsroom](https://newsroom.spotify.com/2025-09-24/dj-software-integration-premium),
[MusicRadar](https://www.musicradar.com/music-tech/dj-software/after-5-years-spotify-streaming-returns-to-dj-software-but-theres-a-catch),
[Digital DJ Tips](https://www.digitaldjtips.com/spotify-comes-to-dj-software/).

So the licence does not allow stems, and the only way in is a partner
integration. Capturing a stream to split it is the line not to cross. The
splitting itself is not the problem (HTDemucs runs in real time on a cooled
Pi 5). If streaming matters: a service with a DJ partner programme whose
terms allow stems, checked service by service.

## 3. Tested 2026-11-24: a slot following a firmware deck

freedj-esp's own firmware for the CDJ-2000 and the XDJ-1000MK2 now names
its loaded track by rekordbox ID and serves its stick over NFS (no
dbserver; freedj-esp `aae9858`, `11f22c7`). freedj's LINK load finds the
track through the stick's `export.pdb` when there is no dbserver
(freedj-3000 `624209d`).

On djlink0 (cdj3k-emu's private bridge) with a tap for the emulator
(freedj-esp `cdj2000/tools/emu_djlink.py`, `BOARD=xdj1000mk2` for the XDJ):

```
overdeck run overdeck_follow.toml --aio      # OPENDECK_LINK_IFACE=djlink0 OPENDECK_SERVE=0
  deck 1: a local track
  deck 2: follow = 2   (the emulated XDJ-1000MK2)
```

Deck 2 found track 41 in the XDJ's export.pdb and read it over NFS in
2.5 s (10.6 MB, 4.3 MB/s). freedj alone against the emulated CDJ-2000: its
playhead within ±1.2 ms at every beat, held within 1.2 ms on pause, resumed
on PLAY.

### Found on the way (open)

- **Overdeck's deck 2 takes Link player 2 as a fixed number.** The plugin
  numbers its decks by start order ("Deck 1 keeps its MENU PLAYER No.; the
  others take their number", freedj-3000 `crates/plugin/src/lib.rs`), so
  with a real player 2 on the network deck 2 claims 2 as well, and the
  peer table flips between the two. A FOLLOW deck already takes AUTO
  (`link_number`). Fix: any plugin deck after the first takes AUTO too, or
  claims its number only when no player holds it. Worked round in the test
  by putting the follow slot second, so both decks took AUTO (4 and 5).
- **freedj-esp's firmware does not defend its player number.** A real
  player answers another's claim to its number; ours ignores claims, so
  an AUTO claimer that has not yet heard its keep-alive can take its
  number. Add the defence to `p4/deck/main/prolink.cpp` (shared with the
  P4 deck).
- `OPENDECK_PLAYER` is not read by the plugin (only the desktop app and
  mobile), so it cannot pin plugin decks' numbers.
