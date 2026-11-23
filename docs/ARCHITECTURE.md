# Overdeck architecture

Overdeck is a host for decks. A product is a config: a Pi 5 with two jogs
runs two freedj-3000 decks; the same Pi with four runs four; a laptop DAW
runs the same decks as plugins. freedj stays a deck (no mixer: routing and
mixing belong to whoever hosts the decks).

```
            ┌──────────────────────── Overdeck (one process) ───────────────────────┐
 hardware   │  surface drivers          router            decks (plugins)          │
 ─────────► │  gpio · i2c · spi ──ODP──► deck map ──ODP──► freedj (CLAP)  ──audio─┐  │
 jogs, keys │  midi · usb · wire        (config)           freedj (CLAP)  ──audio─┤  │
            │        ▲                                      unit (built in) ─audio─┤  │
 lamps,     │        └──────────ODP (lamps, levels)─────────────┘                 │  │
 screens ◄──│  display sinks ◄── frames (shared buffers) ◄── decks                 ▼  │
            │  hdmi · spi lcd                                     engine: route → device
            └───────────────────────────────────────────────────────────────────────┘
```

## 1. Who owns the hardware: the host does

**Decision: every bus belongs to the host. Decks never open SPI, I2C or GPIO.**
A deck gets *what the controls mean* (ODP), never *how they were read*.

Why the host and not the deck:

- **One owner per bus.** Two decks on one Pi share an SPI bus, an I2C bus and
  a GPIO chip. If each deck opened them, they'd fight over chip selects and
  pins. With one owner there's one schedule and one place for timing.
- **Decks stay portable.** The same freedj plugin runs in Overdeck on a Pi,
  in REAPER on a laptop, and later in a hardware unit over USB. A deck that
  read SPI itself would only run where that SPI is.
- **Real-time is easier.** Polling a quadrature jog at 1 kHz or an SPI panel
  at frame rate is the host's job, on its own thread. The deck's audio thread
  only ever sees a ring of ODP events.
- **The work isn't lost.** freedj already reads hardware in-app: the
  CDJ-3000 sub-CPU over SPI, the XDJ panel, PCF8574 key boards from
  `controllers.toml`. That code moves into Overdeck surface drivers (or a
  crate both share). freedj-the-appliance keeps using it, and Overdeck gets
  it too.

**Passthrough, the escape hatch.** Some panels have a protocol so tied to
one deck that splitting it is pointless. For those a driver can *lease* a
raw bus to one deck, declared in the config. It's the exception and it
costs that deck its portability. The aim is that every panel ends up as a
driver.

**Abstraction level.** Drivers speak normalized controls (`Play`, `Jog(ticks)`,
`Pitch(%)`, `Key(HotCue(3))`), not raw bits. A jog is ticks whether it came
from a quadrature encoder, an AS5600, an optical fin or a MIDI controller.
`crates/odp` defines the vocabulary; `crates/surface` defines the `Driver`
trait.

## 2. ODP: one protocol, every transport

`crates/odp` is the contract between hardware and decks: `Input` (deck,
control) going in, `Output` (lamps, levels, frame-ready) coming back. It
has a fixed little binary codec (`[len][kind][deck][payload]`) and is
`no_std`, so a microcontroller speaks the same frames as the Pi.

ODP is carried every way a deck can be reached:

| Carrier | Where | Status |
|---|---|---|
| CLAP parameter events | freedj's plugin today (Play, Cue, Touch, Pitch, Platter) | **working** (`ParamAdapter`) |
| CLAP extension `com.audiodestrukt.odp` | a deck that takes ODP directly: every key, jog ticks, lamps back | next (needs a small freedj plugin change) |
| bytes over a stream | USB vendor endpoint, UART, socket, SPI to a small board | codec + `surface::Wire` driver working |
| shared memory | a deck in its own process (crash isolation) | later |

Today's freedj plugin only exposes five parameters, so keys past the
transport (hot cues, loops, browse) need the extension. The parameter path
also limits the jog: Platter is a position with a ±8 s range, so a long
spin snaps back. The extension sends ticks, which removes both limits. It's
the first freedj-side change to make.

## 3. Screens

Today each freedj deck opens its own window (its winit loop, inside the
plugin). That's fine on a desktop. On a Pi kiosk (cage shows one window)
it isn't: four decks means four windows fighting for one screen, and no way
to put a deck's screen on the GC9A01 in its jog.

The plan is an ODP display extension. The host hands each deck a
framebuffer per screen (size, format) and the deck renders into it
offscreen, instead of to a window. The host's display sinks then put frames
where they belong:

- **HDMI:** one compositor layout, e.g. four deck screens tiled on one 1080p
  panel;
- **SPI LCDs:** each jog's round screen. This is the host's job for the
  same bus reasons as in §1.

freedj already renders offscreen for screenshots (`OPENDECK_SCREENSHOT`), so
the renderer half exists. It needs a "render to this buffer" entry point.

## 4. Small boards: units, a lighter plugin

CLAP is a light C ABI. What's heavy about freedj-as-CLAP is freedj itself
(wgpu, egui, 160 MB). A Pi Zero or an RP2350 can't load that, and an MCU
can't load anything dynamically.

So the second plugin kind is a **unit**: a Rust trait, not a shared library.

```rust
pub trait Unit {
    fn activate(&mut self, sample_rate: u32, max_block: usize);
    fn input(&mut self, c: odp::Control);              // controls, as ODP
    fn process(&mut self, out: &mut [[f32; 2]]);       // one block, no allocation
    fn output(&mut self, sink: &mut dyn FnMut(odp::Output)) {}   // lamps, levels
}
```

- **Linked in, not loaded.** A board's firmware or Overdeck's own binary
  registers its units at build time; nothing needs dlopen.
- **The same unit everywhere.** On a desktop a thin wrapper exposes a unit
  as CLAP (nih-plug does the work), so it runs in a DAW. In Overdeck a unit
  sits beside CLAP decks in the same engine. On an MCU it's the firmware's
  audio callback.
- **What fits:** a WSOLA deck engine (freedj's pure-Rust stretcher already
  runs at 3.6 % of a Pi Zero W core), samplers, the jog/transport rules,
  meters. A full freedj deck with its GPU UI stays a CLAP plugin on
  Pi 5-class boards.
- **Later, if wanted:** WebAssembly units for sandboxed, downloadable
  units on mid-size boards. I wouldn't start there: the DSP cost and
  real-time behaviour need measuring first.

This also gives freedj a cleaner core. Lifting the deck engine into
`Player`/`Output`/`Clock` crates (freedj's player-crate plan) is the same
cut as "a deck as a unit".

## 5. Over USB, Overbridge-style

Elektron's Overbridge is proprietary and undocumented. Its machines show up
in a DAW as plugins, with multichannel audio and parameter control over
USB, via Elektron's own driver and engine. We don't know how it works
inside and we won't take it apart (the same clean-room rule as for
Pioneer's firmware). Here is how Overdeck would get the same effect with
open pieces.

**The board is a USB composite gadget.** A Pi 4/5 USB-C port, a Pi Zero, or
an RP2350 runs in device mode (Linux `configfs` gadget; TinyUSB on an MCU)
and exposes:

1. **USB Audio Class 2**, class-compliant, needing no driver anywhere:
   the deck's stereo out (and stems, as more channels) to any computer;
2. **a vendor-specific bulk interface** carrying ODP frames both ways
   (controls in, lamps and levels out), plus display frames on their own
   endpoint;
3. optionally **USB MIDI**, so a plain DAW can map it with no plugin at all.

**On the computer**, a tiny "Overdeck link" plugin (CLAP/VST3) opens the
vendor interface with libusb and makes the hardware deck look like a local
one: its parameters are ODP and its GUI shows the deck's display frames.
The audio arrives through the OS as UAC2, with no clock to fight. A later
version can carry audio inside the vendor stream as Overbridge does, at the
price of resampling against the host clock.

The hard part of Overbridge-style integration is clocking and latency, not
the protocol. Starting with class-compliant UAC2 for audio and keeping the
custom part to control and display avoids almost all of it.

## 6. What runs today

- `overdeck render`/`run` loads freedj-3000's CLAP, runs N decks, each
  with its own track (written into the plugin's state) and its own output
  channels, driven by scripted ODP inputs.
- An end-to-end test (`OVERDECK_E2E=1 cargo test`) runs two real decks: deck
  0 silent until its PLAY at 3 s, deck 1 silent until its PLAY at 8 s,
  each on its own channels of the mix.
- Hosting decks found two freedj bugs, now fixed in freedj 8fb195d. One is
  very likely REAPER's crash at quit:
  - every deck had its own wgpu instance;
  - plugin `destroy` returned before the deck was gone.

**Known limits:**

- **Network services.** Two decks in one process share one set of network
  services: the second deck's media server can't bind the dbserver port.
  Each deck still claims its own Link player number.
- **Windows.** Decks open their own windows (§3).
- **Not on a Pi yet.** Nothing has been run on a Pi 5. Overdeck is pure
  Rust plus cpal (ALSA), but freedj's CLAP has to be built for aarch64.

## 7. Order of work

1. **Pi 5 bring-up:** build OpenDeck.clap for aarch64 and run two decks
   from `overdeck run` on the Pi's audio, under labwc.
2. **The ODP CLAP extension in freedj:** every key, jog ticks, lamps back.
3. **Surface drivers:** GPIO quadrature for the mini jog's two opto
   sensors, PCF8574 keys, MIDI; then the SPI panels moved over from freedj.
4. **Display extension + sinks:** decks offscreen; HDMI tiling; GC9A01 per
   jog.
5. **Units:** the trait, a WSOLA deck unit, the CLAP wrapper.
6. **USB gadget:** UAC2 + ODP vendor interface on a Pi/RP2350; the link
   plugin.
