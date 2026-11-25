# HasteClip Frame Blending for OBS

A filter for [OBS Studio](https://obsproject.com) that gives a Game Capture, Window Capture or Display Capture [HasteClip](https://github.com/HasteClip/hasteclip)'s frame blending. The picture is captured several times faster than OBS's frame rate, and the pictures in each frame's time are blended into it, for smooth motion like motion blur.

It works live. You don't need to set OBS to 240 fps, and it goes into the stream, recordings, the replay buffer and the virtual camera alike.

```
Windows: works; built and tried in OBS 32.2.2 (Game Capture, Window Capture and Display Capture)
macOS:   not yet (it would capture with ScreenCaptureKit, up to the display's refresh rate)
Linux:   not yet (it would capture through PipeWire, up to the display's refresh rate)
```

## Installing

Download it from **[hasteclip.com/obs](https://hasteclip.com/obs)** (or the [latest release](https://github.com/HasteClip/hasteclip-obs/releases/latest)). It needs OBS Studio 30 or later on Windows 10 or 11, drawing with Direct3D 11 (OBS's default).

- **The installer** (`HasteClip-Frame-Blending-win-Setup.exe`) puts it in `C:\ProgramData\obs-studio\plugins\hasteclip_obs`, where OBS looks for plugins installed for everyone on the PC. OBS's own updates leave it there. If OBS is open, the installer asks to close it first.
- **The zip** (`HasteClip-Frame-Blending-win.zip`) holds that same `hasteclip_obs` folder: extract it into `C:\ProgramData\obs-studio\plugins`.

Then start OBS again. To remove it, uninstall *HasteClip Frame Blending* in Windows' Settings → Apps, or delete the folder.

## Using it

Right-click a **Game Capture**, **Window Capture** or **Display Capture** → **Filters** → **+** → **Frame Blending**.

- **Frame blending**: how fast it captures. The choices are whole multiples of OBS's frame rate, so every frame gets as many pictures: *From 240 fps (×4)* at 60 fps. When you change OBS's frame rate, it keeps capturing at about the same rate (240 fps is ×8 at 30 fps).
- **Blend**: how a frame weighs its pictures. With *Even*, they all count the same: a frame shows just its own moment, and a moving thing's trail ends sharply where the next frame's starts. With *Soft*, the middle of the frame counts most and fades into the frames either side. One frame hands over smoothly to the next, a little blurrier.
- The line below says what it's doing, or why it isn't blending.
- Turn it off with the filter's eye.

Put it **first** in the source's filter list: it draws its own capture, so filters above it are skipped.

### Which sources, and how fast

- **Game Capture** works best. It takes the game's own pictures from the hook OBS puts in the game, as fast as the game draws them, up to 2000 fps. The game's frames are copied a little more often than OBS alone would copy them, which costs the game a little.
- **Window Capture** and **Display Capture** get what Windows' capture delivers: the pictures the screen shows, up to its refresh rate. On a 240 Hz display that means up to 4 pictures a frame at 60 fps.
- Other sources (media, browser, camera, capture cards) can't be blended: OBS hands a filter one picture per frame, and nothing faster.

The blended picture runs about one and a half OBS frames behind the game's sound (25 ms at 60 fps, a little more with Soft), because each frame's blur reaches back over its own time. If that shows, give the game's audio a Sync Offset to match (Edit → Advanced Audio Properties).

## How it works

OBS draws every source once per frame, so a filter alone never sees the pictures between OBS's frames. OBS's Game Capture also limits its hook to copying the game at twice OBS's frame rate. This filter runs its own capture of the same game, window or display:

- **Game Capture:** HasteClip's game capture client joins the hook OBS loaded into the game. Nothing new goes into the game, and the copy rate is kept fast enough for the blend.
- **Window and Display Capture:** Windows' capture.

The blending is HasteClip's. The pictures are timed as they come and kept halfway between the game's pictures, so a game running at the capture rate never gets a picture repeated or skipped. They are averaged as light, in 16-bit floating point. Each finished frame goes to OBS as a shared Direct3D 11 texture, on OBS's own frame clock.

It blends at the size your canvas shows the source, not bigger. A 1440p game on a 1080p canvas is shrunk to 1080p as it's captured, then blended. That's a lot less work, as most of the work is done for every pixel. The scene doesn't change: the frame is drawn at the source's own size. A source zoomed in past filling the canvas comes out a little softer. While another canvas has video (Aitum Vertical's, while it streams or records), the source is blended big enough to fill that one too.

## Building

Rust, on Windows. The capture and blending come from HasteClip's `capture` crate, fetched from GitHub (its `feat/obs-plugin` branch for now, see `Cargo.toml`):

```
cargo build --release
```

To build against a HasteClip checkout of your own (to change the capture side too), put this in `.cargo/config.toml` here (it's ignored by git):

```toml
[patch."https://github.com/HasteClip/hasteclip"]
capture = { path = "../hasteclip/crates/capture" }
```

Copy `target\release\hasteclip_obs.dll` into `C:\ProgramData\obs-studio\plugins\hasteclip_obs\bin\64bit\`, or into OBS's own `obs-plugins\64bit` folder (not both: OBS would load it twice).

## Releasing

Set the new version in `Cargo.toml` (`version`), commit, then push a tag `vX.Y.Z` with that version. The release workflow (`.github/workflows/release.yml`) checks that they match, runs the tests, builds the plugin for release with HasteClip's `capture` at the commit `Cargo.lock` pins, and publishes the tag's release with the installer and the zip. Their names stay the same from release to release, as hasteclip.com/obs links to them through `releases/latest/download/`.

Run by hand (Actions → Release → Run workflow), it builds both and publishes nothing: they're kept on the run, as `release`.

## Licence

GPL-3.0-or-later, as HasteClip. OBS Studio is GPL-2.0-or-later. The HasteClip name and icon aren't part of the licence: see [TRADEMARKS.md](TRADEMARKS.md).
