$env:STREMIO_LIBMPV_REFRESH = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
cargo build --release --target x86_64-pc-windows-msvc
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" /DSIGN "/Sstremiosign=`$qsigntool`$q sign /fd SHA256 /t http://timestamp.digicert.com /n `$qSmart Code OOD`$q `$f" "setup\Stremio.iss"
