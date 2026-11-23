# Changelog

## [Unreleased]

### Added
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
