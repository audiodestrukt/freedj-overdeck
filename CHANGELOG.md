# Changelog

## [Unreleased]

### Added
- The mixer's panel (`overdeck-panel`) shows two channel strips (Trim,
  High, Mid and Low knobs, a fader), a Master knob and a crossfader. It
  shows the mixer's live values, so scripted automation moves it, and
  sends your moves back.
  - With `panel = true`, Overdeck opens the panel itself and talks to it
    over a local socket, one JSON object per line.
  - The panel is a separate process because the decks already hold the
    process's one winit event loop.
  - When docked, it sits at the bottom centre.
  - The CLAP host reads parameter values and their text back from plugins.
- `dock = true` puts the decks' windows side by side across the screen
  (freedj's `OPENDECK_TILE`). On a Wayland desktop the decks go through
  XWayland, since Wayland doesn't let a client place its windows. This is
  a stopgap until the decks draw into Overdeck's own window.
- A reference DJ mixer plugin (`crates/mixer`, CLAP and VST3) for testing
  decks. It has Trim, a three-band isolator EQ with full kills, channel
  Faders, a constant-power Crossfader and Master, and unit-tested DSP.
  `tools/install-mixer.sh` installs it for DAWs.
- Overdeck's mixer stage: decks feed a mixer plugin's input ports, and the
  mixer goes to the device.
- `[[automate]]`: scripted parameter moves for any plugin, set or ramped
  in the parameter's own units.
- The CLAP host gives plugins audio inputs, with no allocation per block.
- `demos/two-deck-mix.toml`: a crossfade with a bass swap. An end-to-end
  test checks the crossfader and EQ on real decks.
- Overdeck, a plugin host for decks: loads freedj-3000's CLAP plugin and
  runs any number of decks in one process. Each deck gets its own track
  (written into the plugin's state) and its own output channels.
  `overdeck run` plays to the sound card, `overdeck render` writes a mix
  and one stem per deck, and `overdeck list` shows a bundle's plugins and
  parameters.
- ODP, the Overdeck protocol: normalized deck controls in, and lamps,
  levels and frame-ready notices out, with a `no_std` binary codec.
- The `surface` hardware abstraction: the `Driver` trait, a scripted
  driver, and a driver that reads ODP frames from any byte stream.
- An end-to-end test with two real freedj decks, each silent until its
  own PLAY and each on its own channels.
- docs/ARCHITECTURE.md: the host owns the hardware, ODP carriers,
  screens, units for small boards, and USB in the style of Overbridge.
