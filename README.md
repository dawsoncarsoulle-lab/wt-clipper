# WT Clipper

WT Clipper is a Linux desktop clip recorder for War Thunder. It keeps a rolling replay buffer, watches the local War Thunder HTTP API, detects personal kills, and saves WebM clips automatically with video and system audio.

It is not affiliated with, endorsed by, or sponsored by Gaijin Entertainment or War Thunder.

> TODO image: add a wide screenshot of the Dashboard while War Thunder is connected, buffer at 100%, and recent events visible. Suggested path: `docs/images/dashboard-connected.png`.

## Features

- Automatic clips on personal War Thunder kills.
- Multi-kill grouping into a longer clip when kills happen close together.
- Manual clip button.
- Rolling replay buffer with configurable duration.
- System audio capture through PulseAudio/PipeWire monitor sources.
- Wayland/COSMIC capture through xdg-desktop-portal and PipeWire.
- X11 window capture through `ximagesrc` with automatic War Thunder window detection.
- Clip library with hover-to-play video previews.
- Diagnostics page for capture, GStreamer, audio, War Thunder localhost, and writable directories.
- MIT licensed.

> TODO image: add a screenshot of the Clips library showing video previews. Suggested path: `docs/images/library-previews.png`.

> TODO image: add a screenshot of the Diagnostics page with all checks OK. Suggested path: `docs/images/diagnostics-ok.png`.

## Current Status

WT Clipper is usable, but still early. It has been developed and tested primarily on Pop!_OS with COSMIC and GNOME/X11. Treat releases before `1.0.0` as test builds.

Known focus areas before a public release:

- Broader distro testing.
- Cleaner packaging.
- Better first-run onboarding.
- UI controls for audio device selection.
- More polished release screenshots and demo video.

## Requirements

- Linux.
- War Thunder running with local telemetry available at `http://127.0.0.1:8111`.
- PipeWire/PulseAudio for system audio.
- GStreamer with the plugins used by the capture pipeline.
- For Wayland/COSMIC: `xdg-desktop-portal` and a desktop portal backend.
- For X11: X11 session and `ximagesrc`.

Useful Ubuntu/Pop!_OS packages:

```bash
sudo apt install \
  gstreamer1.0-tools \
  gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad \
  pipewire \
  pulseaudio-utils \
  xdg-desktop-portal
```

For GNOME Wayland portal support:

```bash
sudo apt install xdg-desktop-portal-gnome
```

Package names vary by distro.

## Running From Source

Install Rust, Node.js, and system capture dependencies first.

```bash
npm install
npm --prefix frontend install
cargo run --release -- gui
```

The GUI command launches the Tauri app in development mode from the Rust CLI.

For direct Tauri development:

```bash
npm run dev
```

For frontend-only build validation:

```bash
npm run frontend:build
```

For Rust validation:

```bash
cargo check
cargo test
```

## Configuration

Create a default config:

```bash
cargo run --release -- config init
```

Default config location depends on the app config path handling. The main fields are:

```toml
[clip]
seconds = 20
segment_seconds = 2
post_event_seconds = 5
multi_kill_window_seconds = 8
output_dir = "~/Videos/WarThunder Clips"
quality = "high"
fps = 60
video_bitrate_kbps = 20000
source = "window"
keep_segments = false

[war_thunder]
player_name = ""
poll_interval_ms = 300
```

Set `war_thunder.player_name` to your in-game nickname so the auto clipper only reacts to your kills.

Capture source:

- `window`: preferred. On Wayland this asks the portal for a window; on X11 WT Clipper tries to find the War Thunder window automatically.
- `screen`: captures the selected screen/session source.

## Audio Capture

WT Clipper records system audio from the default PulseAudio/PipeWire sink monitor.

To force a specific audio monitor:

```bash
WT_CLIPPER_AUDIO_DEVICE=alsa_output.example.monitor cargo run --release -- gui
```

To disable audio capture:

```bash
WT_CLIPPER_DISABLE_AUDIO=1 cargo run --release -- gui
```

If clips have no audio, run Diagnostics and check:

- `plugin pulsesrc`
- `plugin opusenc`
- `System audio monitor`

## Wayland And X11

Wayland/COSMIC path:

- Uses `xdg-desktop-portal` ScreenCast.
- Uses `pipewiresrc`.
- The first capture may open a desktop selection dialog.

X11 path:

- Uses `ximagesrc`.
- With `source = "window"`, WT Clipper searches X11 windows for War Thunder / `aces.exe`.
- If automatic detection fails, launch War Thunder first or set:

```bash
WT_CLIPPER_X11_WINDOW_ID=0x123456 cargo run --release -- gui
```

## Diagnostics

Run diagnostics from the app or from the CLI:

```bash
cargo run --release -- doctor
```

JSON output:

```bash
cargo run --release -- doctor --json
```

Diagnostics check:

- Current session type.
- Capture backend.
- Portal availability.
- GStreamer plugins.
- X11 War Thunder window detection.
- System audio monitor.
- War Thunder localhost reachability.
- Output and temp directory writability.

## CLI Commands

```bash
cargo run --release -- gui
cargo run --release -- doctor
cargo run --release -- status
cargo run --release -- watch
cargo run --release -- auto
cargo run --release -- record --duration 10 --output /tmp/test.webm
cargo run --release -- buffer
```

The GUI is the intended user experience. CLI commands are useful for debugging and development.

## Building A Release

```bash
npm run frontend:build
cargo test
npm run build
```

Tauri outputs are generated under `src-tauri/target/release/bundle/`.
The default bundle targets are currently `.deb` and `.rpm`. AppImage is tracked as a follow-up release task.

See [RELEASE_CHECKLIST.md](RELEASE_CHECKLIST.md) before publishing a build.

## Troubleshooting

War Thunder is shown as disconnected:

- Start War Thunder and enter a battle/test flight.
- Open `http://127.0.0.1:8111` in a browser and verify it responds.
- Check `/gamechat` and `/hudmsg`.

Wayland portal does not open:

- Install and start `xdg-desktop-portal`.
- Install the backend for your desktop, for example `xdg-desktop-portal-gnome`.
- Re-login after installing portal packages.

X11 captures the whole desktop:

- Use `source = "window"`.
- Make sure War Thunder is running before starting capture.
- Run Diagnostics and inspect `X11 War Thunder window`.
- Set `WT_CLIPPER_X11_WINDOW_ID` manually if needed.

Clips have no audio:

- Run Diagnostics and inspect `System audio monitor`.
- Install `pulseaudio-utils` for `pactl`.
- Set `WT_CLIPPER_AUDIO_DEVICE`.

Clips are not saved:

- Check that the buffer reaches 100%.
- Check output directory permissions.
- Check terminal logs around `[WT] kill detected` and `[CLIP]`.

## Contributing

Useful test matrix:

- COSMIC Wayland.
- GNOME Wayland.
- GNOME X11.
- Steam War Thunder.
- Standalone War Thunder launcher.
- Headphones, speakers, and Bluetooth audio.

Before submitting changes:

```bash
cargo fmt
cargo test
npm run frontend:build
```

## License

MIT. See [LICENSE](LICENSE).
