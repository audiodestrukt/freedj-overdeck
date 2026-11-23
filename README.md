# freedj-overdeck

A plugin host for decks. Overdeck runs any number of
[freedj-3000](../freedj-3000) decks (its CLAP plugin) in one process, owns
the hardware (jogs, keys, panels, screens), and talks to the decks in one
small protocol, ODP. The aim is a Raspberry Pi 5 that is a two- or four-deck
product depending on its config, and the same decks in a DAW.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design:
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
over to B, and pulls A's fader down.

In freedj-3000, `make plugin` builds and installs `~/.clap/OpenDeck.clap`.

## Crates

| Crate | What it is |
|---|---|
| `odp` | the Overdeck protocol: controls in, lamps/levels/frames out, a `no_std` wire codec |
| `surface` | the hardware abstraction: the `Driver` trait; `Script` and `Wire` drivers |
| `clap-host` | a small CLAP host on `clap-sys`: bundles, instances, params, state, processing |
| `overdeck` | the host binary: config, decks, mixer stage, automation, the audio engine, `run` / `render` / `list` |
| `mixer` | the reference DJ mixer plugin (test instrument) |

## Tests

```sh
cargo test                                  # unit tests
cargo build --release -p overdeck-mixer
OVERDECK_E2E=1 cargo test -- --nocapture    # + real freedj decks, alone and through the mixer (needs the plugin and a display)
```

License: GPL-2.0-or-later, as freedj-3000.
