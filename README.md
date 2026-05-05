## Stremio shell fork with few extra features
* Has Discord Rich Presence support
    * Default look:
    ![General view](https://i.imgur.com/jBWs2PC.png)
* Watch Party support through Steam networking
    * See the [Watch Party Guide](wiki/watch-party.md) for hosting, joining, and troubleshooting
* Media Keys and Windows Media Overlay support:
    * Play/Pause
    * Seek forward/backward
* Modern architecture:
    * Both the app and the MPV player are 64-bit
* Unlocked MPV config:
    * You can change the MPV config by editing the `mpv.conf` file in the app's data directory
    * Supports upscaling: 
        * `vf=d3d11vpp:scale=4:scaling-mode=nvidia` for high quality upscaling
        * `vo=gpu` use the GPU video output
        * `gpu-context=d3d11` for Direct3D 11 GPU context
        
    * Supports lua scripts
        * You can add lua scripts to the `scripts` directory in the app's data directory
        * Script example: [To skip intros](https://raw.githubusercontent.com/Loukious/auto_skip_anime_intro/refs/heads/main/auto_skip_anime_intro.lua)



## Support the fork developer
[!["Buy Me A Coffee"](https://www.buymeacoffee.com/assets/img/custom_images/orange_img.png)](https://buymeacoffee.com/loukious)


## Stremio shell: new gen

A Windows-only shell using WebView2 and MPV

Goals:
* Performance
* Reliability
* Easy to ship

In all three, this architecture excels the [Qt-based shell](https://github.com/Stremio/stremio-shell): it is about 2-5x more efficient depending on the use case, as it allows MPV to render directly in the window through it's optimal video output rather than using libmpv to integrate with Qt.

This is due to Qt having a complex rendering pipeline involving ANGLE and multiple levels of composing and drawing to textures, which inhibits full HW acceleration.

Meanwhile in this setup MPV uses whichever pipeline it considers to be optimal (like the mpv desktop app), which is normally d3d11, allowing full HW acceleration.

For web rendering, we use the native WebView2, which is Chromium based but shipped as a part of Windows 10: therefore we do not need to ship our own "distribution" of Chromium.

Finally, this should be a lot more reliable as it uses a much simpler and more native overall architecture.
