$env:STREMIO_LIBMPV_REFRESH = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
cargo build --release --target aarch64-pc-windows-msvc
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" /DSIGN "/Sstremiosign=`$qsigntool.exe`$q sign /fd SHA256 /t http://timestamp.digicert.com /n `$qSmart Code OOD`$q `$f" "setup\Stremio-arm64.iss"
