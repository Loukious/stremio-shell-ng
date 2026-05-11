# Video upscaling (d3d11vpp / VSR scaling)

This fork lets you control MPV via an `mpv.conf` file placed next to the executable. On Windows you can enable GPU upscaling using MPV’s `d3d11vpp` filter.

MPV’s `d3d11vpp` filter exposes `scaling-mode` values documented as:

- `standard` (default)
- `intel`
- `nvidia`

This page includes configs for NVIDIA and Intel, plus an AMD-oriented fallback config (untested).

## Requirements

- Windows
- A recent GPU driver is recommended
- MPV/libmpv with `d3d11vpp` VSR scaling mode support (NVIDIA/Intel modes were added in **mpv v0.39.0**)

Reference (mpv v0.39.0 release notes):
- https://github.com/mpv-player/mpv/releases/tag/v0.39.0

## Step 1 — Create `mpv.conf` next to `stremio-shell-ng.exe`

This fork’s MPV integration auto-loads `mpv.conf` from the **same folder as `stremio-shell-ng.exe`**.

Default install location is typically:

- `%LOCALAPPDATA%\Programs\Stremio\mpv.conf`

### NVIDIA RTX Video Super Resolution (VSR) via `scaling-mode=nvidia`

Requirements:

- **GPU:** GeForce **RTX** GPU (required for `scaling-mode=nvidia`)

Create/edit `mpv.conf` with:

```ini
log-file=mpv.log
vf=d3d11vpp:scale=4:scaling-mode=nvidia
vo=gpu-next
gpu-api=d3d11
gpu-context=d3d11
```

### Intel VSR via `scaling-mode=intel` (untested)

MPV also documents an Intel VSR scaling mode via `scaling-mode=intel`.

This is **untested** in this fork. If you try it, use:

```ini
log-file=mpv.log
vf=d3d11vpp:scale=4:scaling-mode=intel
vo=gpu-next
gpu-api=d3d11
gpu-context=d3d11
```

### AMD VSR (untested)

MPV’s `d3d11vpp` filter does **not** document a dedicated `scaling-mode=amd` value (only `standard`, `intel`, `nvidia`).

If your AMD driver exposes a video upscaling/VSR feature that can be used through the **standard** D3D11 video processing path, the most likely config to try is the default scaling mode.

This is **untested** in this fork:

```ini
log-file=mpv.log
vf=d3d11vpp:scale=4:scaling-mode=standard
vo=gpu-next
gpu-api=d3d11
gpu-context=d3d11
```

Then restart the app.

Tip: if `mpv.log` appears next to `stremio-shell-ng.exe`, the config was picked up.

## Step 2 — Tune `scale=` for your content

The `scale=` parameter in `vf=d3d11vpp:scale=...` controls the upscaling multiplier.

Practical presets:

- **1080p → 4K:** use `scale=2`
- **720p → 4K:** use `scale=3`
- **480p → 1080p (or heavy upscaling):** use `scale=4`

Higher `scale=` values increase GPU work. If you see dropped frames, try lowering `scale=`.

## Optional — NVIDIA App RTX Video Super Resolution (VSR)

If you are using an NVIDIA RTX GPU, NVIDIA exposes RTX Video enhancements in the NVIDIA App.

From NVIDIA’s own description, the controls live under:

- **NVIDIA App → System → Video**

Look for **RTX Video Enhancements / Video Super Resolution (VSR)** and enable it.

Reference (NVIDIA):

- https://www.nvidia.com/en-us/geforce/news/nvidia-app-beta-update-rtx-vsr-hdr-controls-and-more/

## Notes / Troubleshooting

- If you previously enabled Smooth Motion, your `mpv.conf` may be set up for Vulkan. These upscaling configs use **D3D11** for `d3d11vpp`.
- Use `mpv.log` (created next to `stremio-shell-ng.exe`) to confirm MPV is starting with your chosen `vo`/`gpu-*` settings.
