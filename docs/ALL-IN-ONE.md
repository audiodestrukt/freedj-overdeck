# Overdeck on all-in-one decks

How Overdeck becomes a product that runs on an all-in-one DJ unit: two or
four decks, a mixer and a screen in one box. The first targets are the new
open units (Behringer / Klark Teknik's Anima) and our own Pi-based box.
Written 2026-11-22. Nothing here is decided.

## What's out there

**Klark Teknik Anima (Behringer / Music Tribe).**
- A prototype shown in October 2026: four channels, eight pads a deck,
  long pitch faders and a fold-flat touchscreen in the middle.
- On the back: a HARD DISK bay, USB, Ethernet, line and XLR outs, two
  mic inputs.
- Target price $300–400. No spec sheet, SoC or date.
- Pitched as having "no proprietary lock-in": owners can write their own
  software for it.
- Its software isn't published yet. Mixxx's bare-metal roadmap plans
  support for it, so Mixxx is the obvious rival on this box.

**What Music Tribe's open platform looks like: the Kick.** The Kick
groovebox's code is published, and KickEmulator runs it.
- **System:** a Linux arm64 image built with Buildroot, under GPL-2.0.
- **App:** one application in C++ on JUCE, with its own audio engine,
  under AGPL-3.0-or-later.
- **Front panel:** a microcontroller talking to the app over a serial
  packet protocol (`ComProtocol/PktMgr`, also AGPL). That protocol is the
  only thing coupling the app to the hardware.
- **Screen:** a 1024×600 touchscreen driven by the app.

The Anima will very probably be built the same way, so the work splits
cleanly:
- **the image:** our runtime in their Buildroot tree;
- **one surface driver:** their panel protocol;
- **one screen:** all decks on it.

**Closed all-in-ones** (Engine OS units, AlphaTheta's) are not targets. We
don't open or modify their firmware, for the same clean-room rule as for
Pioneer. They stay peers on Pro DJ Link.

**Our own box.** A Raspberry Pi 5 or CM5 in a printed shell: the 5" mini
jogs (hardware/cad/jogring), keys on I2C, round screens in the jogs or one
HDMI panel. Everything is in our hands, so it's the reference product.

## The product: an image, the runtime, a profile

```
┌─────────────────────────────────────────────────────────────┐
│ profile: product.toml (decks, channels, panel, screen layout) │
├─────────────────────────────────────────────────────────────┤
│ content: freedj decks (CLAP) · mixer plugin · library service │
├─────────────────────────────────────────────────────────────┤
│ Overdeck runtime: host · surface drivers · compositor · engine│
├─────────────────────────────────────────────────────────────┤
│ board: kernel, audio codec, GPU, touch, panel MCU link (BSP)  │
└─────────────────────────────────────────────────────────────┘
```

- **Board.** On Music Tribe hardware this comes from their Buildroot tree,
  where Overdeck is one more package and replaces their app. On a Pi it's
  a Buildroot image (or Pi OS lite with cage, as freedj's appliance does
  today).
- **Runtime.** Overdeck, started by systemd, owns the audio device, the
  panel link and the screen.
- **Content.** The freedj deck plugin, a product mixer plugin, and one
  library service for all decks.
- **Profile.** One file says what this box is. For an Anima it might be 4
  decks, a 4-channel mixer, the `mt-pktmgr` panel driver, and 4 decks on
  the touchscreen. For the mini deck: 2 decks, the 2-channel mixer, the
  GPIO jogs and the I2C keys. A new box means a new profile and maybe a
  new driver, never a new deck.

## What a four-channel all-in-one needs that Overdeck lacks

1. **All decks on one screen.** Today each deck opens its own window. An
   all-in-one has one touchscreen. This needs the display extension from
   ARCHITECTURE.md §3: decks draw offscreen, Overdeck lays them out, and
   touches go back to the deck under the finger. It's the most important
   piece. For a weak GPU there's freedj's GL path (`WGPU_BACKEND=gl`,
   already used for the CDJ-3000 work) and its SDL backend.
2. **A product mixer.** The reference mixer is a test instrument. A unit
   needs 4 channels, cue (PFL) to the headphones on a second output pair,
   mic channels, booth out, and meters back to the panel's LEDs. That
   means a mixer plugin with several output ports, which the engine has to
   route. Mixing is Overdeck's business; freedj stays a deck.
3. **One library for all decks.** Today each freedj deck starts its own
   media services, and in one process the second deck's media server
   can't bind its port. On a unit with one drive bay and one browser, the
   library (scan, analysis, the media server) belongs in the host, and
   decks ask it. This also fixes the port clash.
4. **The panel driver.** For Music Tribe hardware: `mt-pktmgr`, the Linux
   side of their serial protocol, mapping its control ids to ODP (jog
   ticks, keys, faders) and ODP lamps back. For our box: GPIO quadrature
   for the opto jogs, PCF8574 keys.
5. **An appliance.**
   - boot straight into Overdeck;
   - a read-only root, with settings on their own partition;
   - A/B updates (RAUC or SWUpdate), so a failed update can't brick it;
   - quick start-up.
6. **A CPU budget.** Four decks need the light time-stretchers (WSOLA,
   Bungee), not R3. Live stems on four decks are out on anything
   Pi-class. Until a SoC is named, the Pi 5 is the yardstick.
7. **Pro DJ Link.** Each deck takes its own player number (freedj does,
   via AUTO), so a 4-deck box shows up as four players.

## Licensing

- **freedj and Overdeck** are GPL-2.0-or-later.
- **Music Tribe's** app and panel protocol are AGPL-3.0-or-later; their
  Buildroot tree is GPL-2.0.
- **A driver written from their protocol code** derives from AGPL code.
  "Or later" lets Overdeck be combined with it, and the combined image
  ships under AGPL-3.0 with its source offered.
- **Keep the boundary visible.** The driver goes in its own crate
  (`surface-mt`) under AGPL-3.0-or-later, so the rest of Overdeck stays
  GPL-2.0-or-later and other boxes aren't affected.
- **No Behringer or Klark Teknik branding** on what we ship, beyond factual
  compatibility.

## Getting there before an Anima exists

1. **Pi 5 bring-up.** Two decks, the mixer and the panel on a Pi 5. This
   is the same arm64 class as Music Tribe's platform, and our reference
   box.
2. **One screen.** The display extension: freedj renders into a buffer,
   and Overdeck lays out N decks and routes touches.
3. **The library in the host.** One scan and one media server; decks
   browse it.
4. **A dry run on Music Tribe's platform with the Kick emulator.** Put
   Overdeck in the Kick's arm64 image in KickEmulator's QEMU mode, with a
   `surface-mt` driver speaking `PktMgr` to its emulated panel, so the
   Kick's transport keys and pads drive a freedj deck. The Kick isn't a DJ
   deck, but this proves the image packaging, the panel protocol and the
   1024×600 screen before an Anima ships.
5. **The product mixer:** 4 channels, cue, mics, meters.
6. **When the Anima's code lands:** a profile and a driver mapping. With
   steps 1–5 done, that should be most of the work.

## Questions for Dan

- **Which box first:** our Pi box (in our hands now) or the Anima (bigger
  audience, no hardware yet)? The order above does the Pi first, since
  everything it needs is also needed for the Anima.
- **Should Overdeck grow a product mixer?** freedj stays a deck either way.
- **Is an AGPL-3.0 image for Music Tribe boxes acceptable?**
- **Should we talk to Music Tribe** about a developer unit or early access
  to the Anima code? That's an outside contact, so it's your call.

Sources:
- [DJ TechTools on the Anima](https://djtechtools.com/2026/10/27/behringer-teases-a-300-400-open-dj-standalone/)
- [Digital DJ Tips on the Anima](https://www.digitaldjtips.com/klark-teknik-anima/)
- [djwork.it prototype report](https://www.djwork.it/en/2026/10/30/klark-teknik-anima-behringer-shows-an-open-standalone-dj-prototype/)
- [Mixxx bare-metal Pi roadmap](https://timetohouse.com/en/articles/mixxx-bare-metal-raspberry-pi-dj)
- [MusicRadar on the Kick's open firmware](https://www.musicradar.com/music-tech/behringer-kick-is-an-mpc-inspired-sampler-designed-with-vibe-coded-firmware-at-its-core-is-this-really-the-future-we-want-for-music-tech-hardware)
- [KickEmulator](https://github.com/KickEm-Tools/KickEmulator)
