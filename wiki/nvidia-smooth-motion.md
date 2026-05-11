# NVIDIA Smooth Motion (RTX 40/50) for this Stremio fork

NVIDIA Smooth Motion is a driver-based frame interpolation feature that can insert an extra frame between rendered frames. When enabled, 24 FPS video content can show up as ~46–48 FPS (depending on your display refresh rate and overlay rounding).

Example (Smooth Motion enabled):

![Smooth Motion example: ~46 FPS](https://i.imgur.com/e8FW482.jpeg)

## Requirements

- **GPU:** GeForce **RTX 40 Series** or **RTX 50 Series** (this is where NVIDIA exposes the Smooth Motion toggle)
- **Software:** Latest **NVIDIA App** + a recent **GeForce Game Ready Driver** (Smooth Motion requires both installed)

## Step 1 — Create `mpv.conf` (Vulkan + gpu-next)

This fork’s MPV integration auto-loads `mpv.conf` from the **same folder as `stremio-shell-ng.exe`**.

Default install location is typically:

- `%LOCALAPPDATA%\Programs\Stremio\mpv.conf`

Create/edit `mpv.conf` with exactly:

```ini
log-file=mpv.log
vo=gpu-next
gpu-api=vulkan
gpu-context=winvk
video-sync=audio
```

Then restart Stremio.

Tip: if `mpv.log` appears next to `stremio-shell-ng.exe`, you know the config was picked up.

## Step 2 — Enable Smooth Motion in NVIDIA App

1. Open **NVIDIA App**
2. Go to **Graphics** → **Program Settings**
3. Select **stremio-shell-ng.exe** (or add/browse to `stremio-shell-ng.exe` if it’s not listed)
4. Scroll to **Driver Settings**
5. Turn **Smooth Motion** **On**

![Enable Smooth Motion in NVIDIA App](https://i.imgur.com/BU4MhaN.png)

If you don’t see the Smooth Motion toggle:

- Confirm you’re on an **RTX 40 / RTX 50** GPU
- Update **NVIDIA App** and your **Game Ready Driver**
- If you’re on an older NVIDIA App build, enable **Early Access / experimental features** in **Settings → About** (NVIDIA has used this to roll out features ahead of full release)

## Verify it’s working

- Play a known **24 FPS** video in Stremio
- Enable NVIDIA’s performance overlay (commonly **Alt+R**) and check FPS
- With Smooth Motion enabled, you should typically see **~46–48 FPS** instead of ~24 FPS

## Reference

- NVIDIA announcement describing Smooth Motion and where to enable it in NVIDIA App:
  - https://www.nvidia.com/en-us/geforce/news/nvidia-app-global-dlss-overrides-rtx-40-series-smooth-motion/
