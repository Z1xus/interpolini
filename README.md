# <img src="assets/interpolini.svg" alt="" width="36" height="36" align="absmiddle"> interpolini

Yet another [svpaghetti](https://github.com/Z1xus/open-svpflow) and [rifatoni](https://github.com/TNTwise/rife-ncnn-vulkan) frontend, this time faster.

![interpolini](https://github.com/Z1xus/interpolini/blob/assets/mockup.png?raw=true)

It does the same thing as [smoothie](https://github.com/couleur-tweak-tips/smoothie-rs) and [blur](https://github.com/f0e/blur) (interpolates the clip to a high frame rate and blends it back down for motion blur), just without [VapourSynth](https://www.vapoursynth.com) or Python.

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

For batch processing you can either just pass multiple clips, or double click interpolini-cli which would open a file picker dialog. The config priority is the following: clip's folder > next to the app > built-in defaults.

## Why it's faster

- Interpolation and blending both happen on the GPU inside [open-svpflow](https://github.com/Z1xus/open-svpflow), so only the blended frames come back to the CPU.
- There is no VapourSynth, so frames don't go through a frame server and Python between the filters.
- [FFmpeg](https://ffmpeg.org) is linked into the app instead of being piped to. The decoder writes into the buffer that the interpolator reads from, and the encoder reads the interpolator's output without a copy.
- The motion search uses a half-size frame by default (Drawback is it costs a bit of quality).
- Optional [TensorRT](https://developer.nvidia.com/tensorrt) support for RIFE (it uses Vulkan by default).

## Build

You need [Rust](https://rustup.rs) and [clang](https://clang.llvm.org) (on Linux also the dev packages for ALSA, fontconfig, Wayland and X11).

```sh
bash .github/build.sh x86_64-unknown-linux-gnu  # Linux
bash .github/build.sh x86_64-pc-windows-msvc    # Windows (in Git Bash from a Visual Studio developer prompt)
bash .github/build.sh aarch64-apple-darwin      # macOS (x86_64-apple-darwin on Intel)
```

The script builds the app and downloads the rest ([open-svpflow](https://github.com/Z1xus/open-svpflow/releases), and the [RIFE plugins](https://github.com/TNTwise/rife-ncnn-vulkan) and [FFmpeg](https://ffmpeg.org) from [precotti](https://github.com/Z1xus/precotti)) into the dist folder.

## Credits

[RIFE](https://github.com/hzwer/ECCV2022-RIFE) and the [ncnn port](https://github.com/TNTwise/rife-ncnn-vulkan) by nihui and TNTwise, and [smoothie](https://github.com/couleur-tweak-tips/smoothie-rs).

Licensed under GPL-3.0.
