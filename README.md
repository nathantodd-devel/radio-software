# Airspy Scanner

A radio scanner for [Airspy](https://airspy.com) software-defined receivers, written in Rust.

Instead of stepping through channels one at a time like a traditional scanner, it receives a
whole band at once (about 9 MHz on an Airspy R2) and demodulates every channel in it
simultaneously, so nothing is missed while it is "somewhere else". It decodes analog FM and
P25 Phase 1 digital voice, and comes as a desktop app and a command-line tool.

It ships with channel lists for San Mateo County, California, and can import any other area.

## Contents

- [Features](#features)
- [Status](#status)
- [Requirements](#requirements)
- [Install](#install)
- [Using the desktop app](#using-the-desktop-app)
- [Using the command line](#using-the-command-line)
- [The channel database](#the-channel-database)
- [Importing channels](#importing-channels)
- [Themes](#themes)
- [Building from source](#building-from-source)
- [Android](#android)
- [Project layout](#project-layout)
- [How it works](#how-it-works)
- [Legal](#legal)
- [License](#license)

## Features

- **Wideband reception.** One tuning covers every channel in the band; all of them are
  demodulated in parallel.
- **Analog FM** with CTCSS (PL) tone squelch, so departments sharing a frequency are told apart.
- **P25 Phase 1** voice, both trunked systems and conventional channels. On a trunked system
  it listens to every site frequency and reads the talkgroup from each call, so it does not
  need to follow the control channel. Encrypted calls are skipped.
- **Three scan modes:** receive one band; hop between bands when the selected systems are too
  far apart; or scan one channel at a time at a low sample rate for less CPU and USB load.
- **Selectable sample rate** (10 or 2.5 MSPS on an Airspy R2).
- **Per-channel controls:** hold, skip, priority (interrupts other traffic) and record.
- **Recording and replay.** Every call is recorded; finished calls can be replayed from the
  activity log.
- **Channel database** (SQLite) with in-app editing, and import from CSV files or directly
  from RadioReference.com.
- **Search and filters** over channels and the activity log.
- **Unit IDs** shown for P25 calls that carry them.
- **Colour themes,** including Light and the four Catppuccin flavours, plus your own as JSON.
- **Portable mode:** keep all data in a folder beside the executable.

## Status

This is a young project. What has and has not been exercised:

| Area | State |
|---|---|
| Linux, Airspy R2, analog FM and P25 Phase 1 | Tested on air |
| Band hopping, one-channel-at-a-time mode, 2.5 MSPS | Tested on air |
| Linux desktop app | Builds and runs; developed without being able to see it, so expect rough edges |
| Windows and macOS builds | Set up in the release workflow; never built or run |
| RadioReference API import | Requests verified against the live service; parsing of real replies untested |
| RadioReference CSV import | Tested only on hand-made samples |
| Android app | Skeleton only: a placeholder screen, not yet seen running |
| Airspy Mini | Should work (sample rates are read from the device); untested |

Not supported: P25 Phase 2, DMR, NXDN, DCS (digital squelch codes), and other SDR hardware.

## Requirements

- An Airspy R2 or Airspy Mini.
- An antenna suited to the bands you want. Weak, hissy audio usually means the antenna.
- **Linux:** `libairspy` and `aplay`.
  - Fedora: `sudo dnf install airspyone_host alsa-utils`
  - Debian/Ubuntu: `sudo apt install libairspy0 alsa-utils`

  These packages also install the udev rule that lets you use the Airspy without being root.
  The desktop app needs Wayland or X11 and a Vulkan-capable GPU.
- **Windows:** nothing extra; the release zip includes the Airspy libraries.
- **macOS:** `brew install airspy`.

## Install

Download an archive for your platform from the
[releases page](https://github.com/nathantodd-devel/radio-software/releases), unpack it, plug in
the Airspy and run `scanner-ui` (or `scanner` for the command line). Each archive has a
`README.txt` with platform notes.

Or [build from source](#building-from-source).

## Using the desktop app

```sh
scanner-ui
```

- **Sidebar:** the systems in your database, grouped by location. Click one to scan it; click
  more to scan them together. Each has an **Edit** button; **New…**, **Import…** and
  **RadioReference…** are at the bottom.
- **Header:** squelch and volume, mute, and **Settings**.
- **Channel list:** each row shows a live signal meter, call count and last-heard time, with
  these buttons:

  | Button | Effect |
  |---|---|
  | Rec | Always save this channel's calls. Also makes it a priority channel. |
  | Pri | Priority: interrupts whatever else is playing. |
  | Hold | Stay on this channel only. |
  | Skip | Ignore this channel entirely. |
  | Edit | Change, move or delete the channel. |

- **Search box:** type to narrow the channels and the activity log; **Active** and **Heard**
  filter to channels transmitting now or heard since the scan started.
- **Activity log:** recent calls with time, signal strength and length. ▶ replays a finished call.

### Settings

| Setting | What it does |
|---|---|
| Scan mode | *One band*, *Hop between bands* (default) or *One channel at a time*. |
| Sample rate | One of the rates your Airspy offers. Lower covers less spectrum with less load. |
| Time on a quiet band | How long to wait on a band with no traffic before moving on. |
| Longest turn on a busy band | A band with constant traffic is left after this long so others get a turn. |
| Receiver gain | Airspy gain, 0 to 21. |
| Keep recordings | On: recordings are kept. Off: deleted when the app closes (except channels marked Rec). |
| Theme | Colours for the app. |

Settings, the selected systems, and each channel's skip, priority and record marks are
remembered between runs.

## Using the command line

```sh
scanner systems                 # list the systems in the database
scanner police                  # scan a system, by name (or part of it) or number
scanner police fire p25         # scan several; hops between bands if they don't fit one
scanner channels p25            # list a system's channels
```

Useful options:

| Option | Meaning |
|---|---|
| `-g`, `--gain N` | Airspy linearity gain, 0-21 (default 17) |
| `-s`, `--squelch DB` | Carrier level over the noise floor needed to open (default 6) |
| `--scan hop\|band\|channel` | Scan mode (default `hop`) |
| `--rate HZ` | Sample rate, e.g. `2500000` (default: the fastest the device offers) |
| `--record DIR` | Save every transmission as a WAV file |
| `--record-marked DIR` | Save only channels marked Rec in the app |
| `--no-audio` | Don't play audio |
| `--dwell SECS`, `--stay SECS` | Band-hopping timing |
| `--hold SECS` | Stay on a channel this long after it goes quiet (default 1.5) |
| `--db FILE` | Use a different channel database |

`scanner --help` lists everything. Each call is logged as it starts:

```
18:24:44 >    TG 865  SMC SO PatPri 1   Sheriff Patrol Primary 1 - South County  +26 dB
18:24:45      TG 709  SMC EMS Disp Red  EMS Dispatch (Red)                       +17 dB
```

`>` marks the call being played; the number is signal strength over the noise floor.

## The channel database

Channels live in a SQLite file, `airspy-scanner/channels.db`, in your user data directory:

| Platform | Location |
|---|---|
| Linux | `~/.local/share/airspy-scanner/` |
| Windows | `%APPDATA%\airspy-scanner\` |
| macOS | `~/Library/Application Support/airspy-scanner/` |

A new database starts with three systems for San Mateo County, California: Police, Fire and
the County P25 system.

**Portable mode:** if a folder named `airspy-scanner` exists beside the executable, the
database, themes and kept recordings go there instead. Create the folder yourself to opt in.

## Importing channels

### From a file

```sh
scanner import channels.csv --name "City Police" --location "Somewhere, ST"
```

or **Import…** in the app. Two kinds of file are understood.

**This program's own CSV format**, one channel per line:

```
# analog FM: frequency in MHz, CTCSS tone in Hz (or -), w (wide) or n (narrow), name, description
488.3125, 114.8, w, San Mateo PD1, San Mateo Police Dispatch

# P25 conventional channel: frequency, NAC in hex (or -), p25, name, description
453.1000, 293, p25, Example PD, Digital dispatch

# P25 trunked system: every site frequency, then the talkgroups
p25, 772.03125
p25, 770.03125
tg, 865, SO Patrol 1, Sheriff Patrol Primary 1
```

Order matters: when several channels are active at once, the one listed first is played.
The files in [`crates/scanner/channels/`](crates/scanner/channels) are complete examples.

**RadioReference CSV exports:** a conventional frequency list, or a trunked system's talkgroup
list together with its site list:

```sh
scanner import talkgroups.csv --sites sites.csv --name "County P25"
```

### From RadioReference directly

```sh
export RR_USERNAME=... RR_PASSWORD=... RR_APP_KEY=...
scanner radioreference system 6919     # a trunked system, by its system ID
scanner radioreference county 223      # a county's conventional channels
```

or **RadioReference…** in the app. This needs a RadioReference premium subscription and an
application key, which RadioReference issues to developers on request. The IDs are the numbers
in the page addresses (`radioreference.com/db/sid/6919`, `.../db/browse/ctid/223`). The app
remembers your username and key, never your password.

In every case only analog FM and unencrypted P25 Phase 1 are imported; the importer reports
what it left out.

## Themes

Built in: Default, Light, and Catppuccin Latte, Frappé, Macchiato and Mocha. To add your own,
put a `.json` file in the `themes` folder beside the channel database and pick it in Settings.
Every key is optional:

```json
{
  "background": "#f6f7f9",
  "panel": "#ebedf1",
  "raised": "#d9dde4",
  "border": "#c5cad3",
  "text": "#1d2129",
  "muted": "#5d6675",
  "accent": "#178a5a",
  "hold": "#b27300",
  "priority": "#2f6fd6",
  "error": "#c9392f"
}
```

The built-in themes in [`crates/scanner-ui/themes/`](crates/scanner-ui/themes) can be copied
as starting points.

## Building from source

You need a recent stable Rust toolchain.

```sh
cargo build --release
./target/release/scanner-ui      # or ./target/release/scanner
cargo test --workspace
```

On Linux the desktop app links against Wayland, xkbcommon, fontconfig and freetype:

- Fedora: `sudo dnf install wayland-devel libxkbcommon-devel fontconfig-devel freetype-devel libxcb-devel`
- Debian/Ubuntu: `sudo apt install libwayland-dev libxkbcommon-dev libfontconfig1-dev libfreetype-dev libxcb1-dev`

By default the Linux app is Wayland-only. For X11 as well, install `libxkbcommon-x11`'s
development package and build with `--features scanner-ui/x11`.

Things to know:

- `Cargo.lock` pins `libc` to 0.2.189, because a later release breaks one of GPUI's
  dependencies. A blanket `cargo update` will break the build.
- CI builds with `--locked` and checks `cargo fmt` and `cargo clippy -- -D warnings`.
- Pushing a tag like `v0.3.0` runs the release workflow, which builds Linux, Windows, macOS
  and Android packages and publishes a GitHub release.

## Android

`mobile/` holds the beginnings of an Android app, built on
[gpui-mobile](https://github.com/itsbalamurali/gpui-mobile). Today it is a skeleton that shows
a placeholder screen; the scanner is not wired in.

It is a separate cargo project and expects a clone of gpui-mobile beside it:

```sh
git clone https://github.com/itsbalamurali/gpui-mobile
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk          # and install the Android NDK via Android Studio

cd mobile
cargo ndk -t arm64-v8a -t x86_64 -P 26 -o android/gradle/app/src/main/jniLibs build
cd android/gradle && ./gradlew assembleDebug
```

Known issue: on the Android emulator with hardware graphics the app stays on its splash
screen, because the renderer rejects the emulator's Vulkan driver. Use a real phone, or try
the emulator's software graphics mode.

## Project layout

| Path | What it is |
|---|---|
| `crates/radiocore` | DSP: wideband channelizer, FM demodulator with tone squelch, P25 Phase 1 decoder |
| `crates/scanner` | The engine (receive loop, scheduling, recording), channel database, importers, and the `scanner` CLI |
| `crates/scanner-ui` | The desktop app, built with [GPUI](https://www.gpui.rs) |
| `crates/drivers/airspy` | Airspy access through the system's `libairspy`, loaded at run time |
| `crates/drivers/audio` | Audio output: `aplay` on Linux, the native audio API elsewhere |
| `mobile` | Android app skeleton (separate cargo project) |
| `.github/workflows` | CI, Android CI and the release build |

## How it works

1. The Airspy is tuned to the middle of a band and streams complex samples at 10 MSPS.
2. A channelizer takes one large FFT per millisecond and, for each channel, inverse-transforms
   just the bins around it. That yields a 48 kHz stream per channel for the cost of a single
   forward FFT, however many channels there are.
3. Each analog channel is FM-demodulated, gated by a carrier squelch and a CTCSS tone detector,
   filtered and resampled to 16 kHz audio.
4. Each P25 frequency is demodulated (C4FM/CQPSK), framed and error-corrected; the talkgroup is
   read from the voice frames and the speech is decoded with the
   [`blip25-vocoder`](https://crates.io/crates/blip25-vocoder) crate.
5. A scheduler picks which open channel goes to the speaker: priority channels first, then the
   order they appear in the database, holding briefly after each call for a reply.

When the selected systems span more than one band, the same loop retunes between bands,
staying on one while it has traffic.

## Legal

Listening to unencrypted public-safety radio is legal in most of the United States, but laws
vary by state and country, particularly about using a scanner in a vehicle or while committing
a crime, and about repeating what you hear. Check the rules where you are. This software does
not decrypt anything.

P25 voice decoding uses a third-party vocoder crate that carries its own patent notice; this
project uses only its full-rate (Phase 1) codec. IMBE and AMBE are trademarks of Digital Voice
Systems, Inc. Channel data shipped with the project was compiled from RadioReference.com.

## License

Airspy Scanner is free software, licensed under the
[GNU Affero General Public License, version 3](LICENSE).

In short: you may use, study, change and share it, provided that anything you distribute, or
run as a service for others over a network, comes with its source code under the same licence.
The software comes with no warranty. The [LICENSE](LICENSE) file has the full terms.

Parts that come from elsewhere keep their own licences, all compatible with the above:

- The Android skeleton in `mobile/` includes Java sources from
  [gpui-mobile](https://github.com/itsbalamurali/gpui-mobile), which is offered under
  GPL-3.0-or-later, AGPL-3.0-or-later or Apache-2.0.
- The Windows release bundles `airspy.dll` (BSD 3-clause), `libusb-1.0.dll` and
  `pthreadVC2.dll` (LGPL 2.1), and Microsoft's Visual C++ 2010 runtime.
- Rust dependencies are under their own licences, mostly MIT or Apache-2.0; GPUI is Apache-2.0.
