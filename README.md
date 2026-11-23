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

`overdeck.toml` lists the decks, their tracks, their output channels, and
optional scripted inputs. See `crates/overdeck/src/config.rs` and
`tests/fixtures/two-decks.toml`.

In freedj-3000, `make plugin` builds and installs `~/.clap/OpenDeck.clap`.

## Crates

| Crate | What it is |
|---|---|
| `odp` | the Overdeck protocol: controls in, lamps/levels/frames out, a `no_std` wire codec |
| `surface` | the hardware abstraction: the `Driver` trait; `Script` and `Wire` drivers |
| `clap-host` | a small CLAP host on `clap-sys`: bundles, instances, params, state, processing |
| `overdeck` | the host binary: config, decks, the audio engine, `run` / `render` / `list` |

## Tests

```sh
cargo test                                  # unit tests
OVERDECK_E2E=1 cargo test -- --nocapture    # + two real freedj decks (needs the plugin and a display)
```

License: GPL-2.0-or-later, as freedj-3000.
