# freedj-overdeck

A plugin host for decks. Overdeck runs any number of
[freedj-3000](../freedj-3000) decks (its CLAP plugin) in one process, owns
the hardware (jogs, keys, panels, screens), and talks to the decks in one
small protocol, ODP. The aim is a Raspberry Pi 5 that is a two- or four-deck
product depending on its config, and the same decks in a DAW.

See [docs/ALL-IN-ONE.md](docs/ALL-IN-ONE.md) for packaging it as an all-in-one product (Anima, our own Pi box), and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design:
- why the host owns every bus;
- ODP and its transports;
- screens;
- units (a lighter plugin for small boards);
- USB in the style of Overbridge.

## Status

Early, but it runs real decks on a desktop. It hasn't run on a Pi yet.

## Use

```sh
cargo build --release
./target/release/overdeck list ~/.clap/OpenDeck.clap        # the plugin and its parameters
./target/release/overdeck run overdeck.toml                  # decks to the sound card
./target/release/overdeck render overdeck.toml --seconds 13 --out out/   # mix.wav + deck<N>.wav
```

`overdeck.toml` lists the decks, their tracks, their output channels, an
optional mixer they feed, and optional scripted inputs (`[[step]]`, deck
controls as ODP) and parameter moves (`[[automate]]`, any plugin's
parameter, set or ramped). See `crates/overdeck/src/config.rs`,
`tests/fixtures/` and `demos/`.

## The reference mixer

`crates/mixer` is a two-channel DJ mixer plugin (CLAP and VST3) for testing
decks. It is not a product: freedj stays a deck, and mixing is the host's
job. It has:
- deck A on the main input and deck B on the aux input;
- per channel: Trim (±12 dB), a three-band isolator EQ (Linkwitz-Riley
  crossovers at 300 Hz and 3 kHz; −26 dB is a full kill), and a Fader;
- then a constant-power Crossfader and Master.

Its DSP has unit tests (flat to 0.1 dB with the EQ flat, kills over 40 dB).
`tools/install-mixer.sh` installs it in `~/.clap` and `~/.vst3`, so a DAW
can use it as well.

```sh
cargo build --release -p overdeck-mixer
./target/release/overdeck run demos/two-deck-mix.toml --seconds 42   # set TRACK_A / TRACK_B in it first
```

The demo starts on A, brings B in to the middle, swaps the bass, crosses
over to B, and pulls A's fader down. With `dock = true` the decks sit side
by side. With `panel = true` the mixer's panel opens below them
(`cargo build --release -p overdeck-panel` first). It moves with the
automation, and you can take any control yourself.

## The all-in-one screen

```sh
cargo build --release
./target/release/overdeck run demos/aio.toml --aio     # set TRACK_A..D in it first
```

This is a 1024×600 screen, as Overdeck would look on a 4-channel
all-in-one. It is one window: the decks open none of their own.
- DECKS: each deck's own screen, live, drawn by the deck itself into a
  buffer Overdeck made for it (the display contract, `crates/display`).
  Touch a deck's screen and the touch goes to that deck. With two decks
  the mixer sits under them: Trim, High, Mid and Low knobs and a fader per
  channel, the crossfader and Master. With four, the screens fill a grid;
- PERFORM: each deck's live three-band waveform (from its actual output)
  and a card with CUE and PLAY that drive the deck;
- BROWSE: tracks to LOAD onto any deck.

Closing it stops Overdeck. `--windows` gives the decks their own windows
again. See docs/ALL-IN-ONE.md.

In freedj-3000, `make plugin` builds and installs `~/.clap/OpenDeck.clap`.

## Real decks beside plugin decks

A slot can mirror a real player on Pro DJ Link instead of playing:

```toml
[[deck]]
plugin = "~/.clap/OpenDeck.clap"
follow = 2                     # the Link player to mirror (1-6)
```

The deck (freedj's FOLLOW mode) loads whatever that player loads, over
LINK, and shows its playhead, PLAY, pitch, MASTER and SYNC on its screen,
in its own window or in the DECKS view. It never plays and only listens on
Link. An XDJ-1000 only reports its beat number, so its mirror is exact at
each beat and runs on at the pitch in between; a CDJ-3000 reports its
playhead in ms. Link carries no audio: for now the real player's sound goes
by cable to your mixer. Taking it in on an audio input, into Overdeck's
mixer, comes next (docs/ARCHITECTURE.md §7).

## Crates

| Crate | What it is |
|---|---|
| `display` | the display contract: a deck draws its screen into a shared-memory frame the host made, and takes the touches back |
| `odp` | the Overdeck protocol: controls in, lamps/levels/frames out, a `no_std` wire codec |
| `surface` | the hardware abstraction: the `Driver` trait; `Script` and `Wire` drivers |
| `clap-host` | a small CLAP host on `clap-sys`: bundles, instances, params, state, processing |
| `overdeck` | the host binary: config, decks, mixer stage, automation, the audio engine, `run` / `render` / `list` |
| `mixer` | the reference DJ mixer plugin (test instrument) |
| `panel` | `overdeck-panel`: the mixer's knobs and faders, live, over the host's panel socket |

## Tests

```sh
cargo test                                  # unit tests
cargo build --release -p overdeck-mixer
OVERDECK_E2E=1 cargo test -- --nocapture    # + real freedj decks, alone and through the mixer (needs the plugin and a display)
```

License: GPL-2.0-or-later, as freedj-3000.
