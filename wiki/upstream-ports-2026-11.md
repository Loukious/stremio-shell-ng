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

The custom runtime stays native and does not require `server.js`. Server engine updates are evaluated in [Loukious/stremio-runtime](https://github.com/Loukious/stremio-runtime). The bundled x64 and ARM64 runtimes retain transactional settings, writable cache-folder validation, safe proxy/download headers and immediate cache-folder changes for new torrents. Active torrents keep their original folders. Runtime endpoints and local-stream recovery use the advertised random port through loopback.

The Windows release workflow checks out runtime source at `846b2443167b19f026bcb15c735d35f18d78dcb6` and builds it with `cargo build --locked --release --target` for each shell architecture, using Rust 1.99.1. Both the shell and runtime use static CRT linking (`-C target-feature=+crt-static`) so they do not require external VC++ runtime DLLs. `.cargo/config.toml` also applies this setting to local Windows builds. `RUNTIME_REPOSITORY` and `RUNTIME_REVISION` in `.github/workflows/build-windows-artifacts.yml` select the source; update the revision when adopting a new tested runtime. Each job stages its fresh runtime build into `bin` or `bin-arm64` before compiling the installer and creating the portable ZIP. The shared native-payload verifier checks each binary's PE architecture and normal/delayed DLL imports, rejecting dependencies on VC redistributables. Portable packaging allows only the native desktop binaries and configuration files. FFmpeg, FFprobe and VC runtime DLLs are removed from this repository and excluded from shell packages. The workflow records the source revision and runtime SHA-256 in the job summary.

The pinned runtime 0.2.0 includes [runtime PR #3](https://github.com/Loukious/stremio-runtime/pull/3): current Web's HLSv2 probe/playback contract, independent video/audio conversion decisions, and rqbit 9.1 with no 90-second deadline on proxy media response bodies. Native desktop playback uses MPV and requires only the runtime executable. Browser probing/conversion requires FFprobe/FFmpeg, which remain bundled in the separate standalone runtime releases together with their sources and licenses. All 55 runtime tests and standalone builds/checks for Windows, Linux and macOS on x64 and ARM64 passed in [the runtime workflow](https://github.com/Loukious/stremio-runtime/actions/runs/43333553247).

Choosing Auto or Manual in the player immediately writes `[AutoSkip] manual_intro_outro` in the adjacent `RPCconfig.ini`; startup restores that value. If saving fails, the player keeps the previous mode and reports the error. Both installers create the template only when the file is absent, preserving mode and other settings during upgrades.

Souvlaki remains the Windows media-session owner. Cargo `libmpv2` is updated to 7.0.1; the fork version is retained. Release-only commits and upstream's additional MPV media session are omitted. Optional GPU profile, shortcut and inspection changes from the earlier audit are outside this port.

Validation includes 99 shell Rust tests with Steam support, including a serialized run with static CRT linking, and six intro-skip JavaScript tests. Static shell release builds and actual Inno Setup 7.1.0 installer compilation passed for x64 and ARM64. Server tests use Node fixtures to exercise delayed output, early exits, timeouts, retries, descendants and listener cleanup; the bundled runtime is a native executable. A stalled HTTP subtitle fixture verifies that media changes can cancel a pending subtitle download. A real queued-MPV-event test verifies that old file events cannot announce readiness for replacement media.

Release-workflow validation includes `actionlint`, static runtime binaries for both architectures, PE headers and DLL import inspection, and both portable ZIP file-list and runtime hash checks. Staging rejects missing or wrong-architecture runtime output, and portable packaging rejects missing runtimes without replacing an existing ZIP. Six verifier fixtures cover OS CRT imports, wrong architectures, ordinary/delayed VC dependencies and extra media tools. Translation downloads use the official directory with an unofficial fallback; Inno Setup 6 and 7 locations and command failures are handled explicitly. ARM64 execution remains a hardware check.

Bluetooth/HDMI reconnects, mixed-DPI monitor movement and ARM64 execution still require tests on the corresponding hardware. Compilation and automated tests do not establish those device-specific behaviors.
