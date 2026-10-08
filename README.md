# <img src="assets/interpolini.svg" alt="" width="36" height="36" align="absmiddle"> interpolini

Yet another [svpaghetti](https://github.com/Z1xus/open-svpflow) and [rifatoni](https://github.com/TNTwise/rife-ncnn-vulkan) frontend, this time faster.

![interpolini](https://github.com/Z1xus/interpolini/blob/assets/mockup.png?raw=true)

It does the same thing as [smoothie](https://github.com/couleur-tweak-tips/smoothie-rs) and [blur](https://github.com/f0e/blur) (interpolates the clip to a high frame rate and blends it back down for motion blur), just without VapourSynth or Python.

It's also a wannabe NLE. There is a timeline with video and audio tracks where you can cut, split and layer clips, drop images on top, move, scale and crop things on the preview, and give each clip its own settings.

## Usage

Open interpolini, drop your clips into the window and press Render.

It assumes you have used svpflow and rife before and know what you're doing. I might add more docs in the future, but for now refer to wonderful guides such as [smoothie's recipe page](https://ctt.cx/video/smoothie/recipe/) and [blur's recommended settings](https://github.com/f0e/blur#recommended-settings-for-gameplay-footage).

There is also a CLI:

```sh
interpolini-cli clip.mp4 another.mkv
interpolini-cli -s interpolation.fps=480 -s output.codec=hevc clip.mp4
```

The -s flag changes any setting from interpolini.ini for that run, and --help lists the rest.

## Why it's faster

- Interpolation and blending both happen on the GPU inside [open-svpflow](https://github.com/Z1xus/open-svpflow), so only the blended frames come back to the CPU.
- There is no VapourSynth, so frames don't go through a frame server and Python between the filters.
- FFmpeg is linked into the app instead of being piped to. The decoder writes into the buffer that the interpolator reads from, and the encoder reads the interpolator's output without a copy.
- The motion search uses a half-size frame by default (Drawback is it costs a bit of quality).
- Optional TensorRT support for RIFE (it uses Vulkan by default).

## Build

You need Rust and clang (on Linux also the dev packages for ALSA, fontconfig, Wayland and X11).

```sh
bash .github/build.sh x86_64-unknown-linux-gnu  # Linux
bash .github/build.sh x86_64-pc-windows-msvc    # Windows
bash .github/build.sh aarch64-apple-darwin      # macOS
```

Run the line for your system. On Windows run it in Git Bash from a Visual Studio developer prompt, and on an Intel Mac use x86_64-apple-darwin. The Windows and macOS lines also work on Linux (the script then gets cargo-xwin, or zig and the macOS SDK).

The app is four things that come from different places, so the script gets each of them and puts them together in the dist folder:

- The app itself is built.
- open-svpflow is downloaded from its [nightly releases](https://github.com/Z1xus/open-svpflow/releases).
- The RIFE plugins and FFmpeg are downloaded from [precotti](https://github.com/Z1xus/precotti). It builds the plugins, and FFmpeg for macOS (nobody ships it there), and keeps the [BtbN](https://github.com/BtbN/FFmpeg-Builds) build of FFmpeg for Linux and Windows.

The releases are made with the same script, each system on its own runner. Every download is pinned by hash, so the releases are reproducible.

## Credits

[RIFE](https://github.com/hzwer/ECCV2022-RIFE) and the [ncnn port](https://github.com/TNTwise/rife-ncnn-vulkan) by nihui and TNTwise, and [smoothie](https://github.com/couleur-tweak-tips/smoothie-rs).

Licensed under GPL-3.0.
