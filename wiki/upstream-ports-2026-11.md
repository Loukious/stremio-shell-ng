# Upstream ports, November 2026

These changes adapt upstream through `d640dc1` (November 4) to the fork. PR #37 and its separate rewind and stale-offer fixes remain in the history.

| Upstream change | Port |
| --- | --- |
| `9709dc7`, `c68f730` | Separate cherry-picks for streamed HTTP reconnection and SDR gamma. |
| `68b6526`, `e8c3d2e`, `268a3e2`, `6c62661` | Exact WebUI origins, parsed external URLs, local-file drop handling and finite `start=+NUM` playback options. Custom player commands remain supported. |
| `d12a86c` | Claim the instance pipe before GUI creation; retry commands while the primary instance starts. Queue media requests until the interface is ready. |
| `f31a85c`, `19af450`, `8cd6deb` | Supervise the native runtime asynchronously with a startup deadline, cleanup before retry, bounded WebUI retries and cancellation of obsolete error actions. |
| `0b50ff0` | Use null audio fallback and reload the output when devices return. |
| `88a8d71`, `5c9d1f0`, `43e2a1b` | Correlate first-frame readiness with media transitions; load external ASS subtitles through a separate, cancellable MPV client. |
| `1a7470b` | Embed per-monitor v2 DPI awareness and apply suggested main-window and PiP bounds. |
| `39a332e`, `b68f2d1` | Persist native interface zoom and expose a folder picker restricted to the current local server. |
| `71e8645`, `dc97443`, `2ced528` | Read actual torrent bytes, normalize local paths and share installer associations. Discover external players and validate playlist handoff, including existing MPV/PotPlayer support. |

The custom runtime stays native and does not require `server.js`. Server engine updates are evaluated in [Loukious/stremio-runtime](https://github.com/Loukious/stremio-runtime), preserving disabled transcoding and HTTP/DHT port recovery. The bundled x64 and ARM64 runtimes include implementation `ea945013e8f8f6e0df946568cf3f992e1a9ea39e`: transactional settings, writable cache-folder validation, safe proxy/download headers and immediate cache-folder changes for new torrents. Active torrents keep their original folders. Runtime endpoints and local-stream recovery use the advertised random port through loopback.

The Windows release workflow now checks out runtime source at `da0263ced209b9601d82ffae2c266056d98d4d21` and builds it with `cargo build --locked --release --target` for each shell architecture. The runtime uses static CRT linking (`-C target-feature=+crt-static`) so it does not require an external VC++ runtime DLL. `RUNTIME_REPOSITORY` and `RUNTIME_REVISION` in `.github/workflows/build-windows-artifacts.yml` select the source; update the revision when adopting a new tested runtime. Each job checks the executable's PE architecture and stages its fresh build into `bin` or `bin-arm64` before compiling the installer and creating the portable ZIP. Both distribution formats include `stremio-runtime.exe`; portable packaging fails if it is missing. The workflow records the source revision and runtime SHA-256 in the job summary.

Souvlaki remains the Windows media-session owner. The existing libmpv dependency and fork version are retained. Release-only commits and upstream's additional MPV media session are omitted. Optional GPU profile, shortcut and inspection changes from the earlier audit are outside this port.

Validation includes 88 shell Rust tests with Steam support, 86 without Steam, six intro-skip JavaScript tests, 19 runtime tests, and release builds for x64 and ARM64. The installer scripts pass Inno Setup compiler checks. Server tests use Node fixtures to exercise delayed output, early exits, timeouts, retries, descendants and listener cleanup; the bundled runtime is a native executable. A stalled HTTP subtitle fixture verifies that media changes can cancel a pending subtitle download. A real queued-MPV-event test verifies that old file events cannot announce readiness for replacement media.

Release-workflow validation includes `actionlint`, explicit-target runtime builds with static CRT linking for both architectures, PE headers and DLL import inspection, and portable ZIP file-list and runtime hash checks using the fresh source builds. Staging rejects missing or wrong-architecture runtime output, and portable packaging rejects missing runtimes without replacing an existing ZIP. ARM64 runtime execution remains a hardware check.

Bluetooth/HDMI reconnects, mixed-DPI monitor movement and ARM64 execution still require tests on the corresponding hardware. Compilation and automated tests do not establish those device-specific behaviors.
