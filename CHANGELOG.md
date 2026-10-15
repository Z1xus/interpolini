# Changelog

## 0.1.16
- New: Dedup is set by the share of repeats it replaces
- New: "interpolini GUI" entry in Send to
- Faster: Dedup without interpolation
- Changed: Save frame is in the playback bar and the preview menu
- Fixed: Crash when switching settings fast
- Fixed: Size estimate counted empty space
- Fixed: Black at the start of the file
- Fixed: Frozen frame in separate files
- Fixed: Audio cut short at the end
- Fixed: Restart note after switching FFmpeg back

## 0.1.15
- New: Save the preview frame
- New: Show saved files in their folder
- New: Split with Ctrl+K and Ctrl+B
- Fixed: Peel specks and blocks in motion

## 0.1.14
- New: Fastest encode effort
- New: Peel masking for RIFE
- New: Sections stay open or closed
- New: Render log in debug info
- Faster: Encoding on cards with two NVENC chips
- Fixed: Blend graph without a selected clip
- Fixed: Lost GPU with two RIFE passes

## 0.1.13
- New: Lossless cut as its own mode
- New: Threads and GPU in debug info

## 0.1.12
- New: Size target with a size estimate
- New: Windows installer
- Faster: Blending

## 0.1.11
- New: Render time after a render
- Changed: Film tuning and dedup are on by default
- Faster: Dedup with SVP
- Fixed: Still mask after RIFE pre-interpolation
- Fixed: Peel on solid overlays

## 0.1.10
- New: Masking with peel or still

## 0.1.9
- Faster: SVP motion search

## 0.1.8
- New: Gain, fades, crossfades and roll trim
- New: Moving a clip trims the clip under it
- New: Preview volume
- New: Blend gamma
- New: Tips during a render
- New: Backspace removes a clip
- Faster: Preview plays through cuts
- Fixed: Sounds cut with their video
- Fixed: Lag when adding many clips
- Fixed: Clips opened after a stopped render

## 0.1.7
- New: VAAPI and Vulkan encoders
- Fixed: AV1 quality

## 0.1.6
- Faster: Preview

## 0.1.5
- Faster: RIFE, opening clips and 4:4:4 clips
- Fixed: SVP memory use

## 0.1.4
- New: Updates from inside the app
- New: Compare the original and the result
- New: Upscaler
- New: TensorRT status and requirements
- New: Shortcut tips

## 0.1.3
- New: File dialog, spinner and colors in the CLI
- New: Close all clips
- Fixed: Cancel and save a new config

## 0.1.2
- New: One audio track by default
- Fixed: Preview of clips with an odd width

## 0.1.1
- First release
