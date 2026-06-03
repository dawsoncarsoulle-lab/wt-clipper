# WT Clipper Release Checklist

Use this checklist before publishing a GitHub release or sharing a build publicly.

## 1. Version And Metadata

- [ ] Update `version` in `Cargo.toml`.
- [ ] Update `version` in `src-tauri/Cargo.toml`.
- [ ] Update `version` in `src-tauri/tauri.conf.json`.
- [ ] Update `version` in `frontend/package.json` if needed.
- [ ] Confirm `LICENSE` is present.
- [ ] Confirm README screenshots are present under `docs/images/`.
- [ ] Add or update release notes.

## 2. Local Validation

Run:

```bash
cargo fmt
cargo check
cargo test
npm run frontend:build
cargo check --manifest-path src-tauri/Cargo.toml
```

Expected:

- [ ] Rust compiles.
- [ ] Frontend compiles.
- [ ] Tests pass.
- [ ] No new warnings except known `egui::Image::rounding` deprecation, until fixed.

## 3. Runtime Smoke Test

Test with War Thunder running:

- [ ] App launches.
- [ ] Diagnostics complete without errors.
- [ ] War Thunder status becomes connected.
- [ ] Buffer reaches 100%.
- [ ] Manual clip saves video.
- [ ] Manual clip includes system audio.
- [ ] Auto kill detection creates an event.
- [ ] Auto kill detection saves a clip.
- [ ] Clip preview plays on hover.
- [ ] Delete removes the clip from the UI and disk.

## 4. Session Matrix

Test at least:

- [ ] COSMIC Wayland.
- [ ] GNOME X11.
- [ ] GNOME Wayland, if available.

For each session:

- [ ] `source = "window"` works.
- [ ] `source = "screen"` works or fails with a clear diagnostic.
- [ ] Audio works.
- [ ] No full-desktop capture occurs when window capture is selected.

## 5. Audio Matrix

Test:

- [ ] Speakers.
- [ ] Wired headset.
- [ ] Bluetooth device, if available.
- [ ] App started before changing audio device.
- [ ] App started after changing audio device.

If audio fails:

- [ ] Run Diagnostics.
- [ ] Check `System audio monitor`.
- [ ] Try `WT_CLIPPER_AUDIO_DEVICE=<monitor>`.

## 6. Build Packages

Run:

```bash
npm run build
```

Expected default outputs under:

```text
src-tauri/target/release/bundle/
```

Check:

- [ ] `.deb` exists, if Tauri generated it.
- [ ] `.rpm` exists, if Tauri generated it.
- [ ] App installs or runs on a clean-ish session.
- [ ] Packaged app can access GStreamer, portals, War Thunder localhost, and audio monitor.

AppImage is intentionally not part of the default bundle targets yet. Track it separately:

- [ ] Investigate AppImage bundling failure on the build machine.
- [ ] Add AppImage back to `src-tauri/tauri.conf.json` once it builds reliably.

## 7. Release Assets

Prepare:

- [ ] Linux package files.
- [ ] `README.md`.
- [ ] `LICENSE`.
- [ ] Three screenshots:
  - [ ] Dashboard connected.
  - [ ] Clips library with previews.
  - [ ] Diagnostics OK.
- [ ] Short demo video:
  - [ ] War Thunder kill.
  - [ ] App receives event.
  - [ ] Clip saved.
  - [ ] Clip playback includes audio.

## 8. Public Release Text

Include:

- [ ] One-line description: automatic War Thunder clip recorder for Linux.
- [ ] Linux-only status.
- [ ] Wayland and X11 support notes.
- [ ] Required dependencies.
- [ ] Known limitations.
- [ ] Disclaimer: not affiliated with Gaijin or War Thunder.
- [ ] Link to troubleshooting section.

## 9. Places To Announce

- [ ] GitHub release.
- [ ] Reddit `r/Warthunder`.
- [ ] War Thunder community Discords.
- [ ] Linux gaming Discords/forums.
- [ ] Personal post with demo video.

Keep the first announcement modest: ask for Linux testers instead of presenting it as finished software.
