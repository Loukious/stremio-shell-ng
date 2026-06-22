use chrono::{Datelike, Local};
use std::{
    env, fs,
    io::Cursor,
    path::{Path, PathBuf},
};

extern crate winres;
fn main() {
    let now = Local::now();
    let copyright = format!("Copyright © {} Smart Code OOD", now.year());
    let exe_name = format!("{}.exe", env::var("CARGO_PKG_NAME").unwrap());
    let mut res = winres::WindowsResource::new();
    res.set_manifest(
        r#"
    <?xml version="1.0" encoding="UTF-8" standalone="yes"?>
    <assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
    <dependency>
        <dependentAssembly>
            <assemblyIdentity
                type="win32"
                name="Microsoft.Windows.Common-Controls"
                version="6.0.0.0"
                processorArchitecture="*"
                publicKeyToken="6595b64144ccf1df"
                language="*"
            />
        </dependentAssembly>
    </dependency>
    </assembly>
    "#,
    );
    res.set("FileDescription", "Freedom to Stream");
    res.set("LegalCopyright", &copyright);
    res.set("OriginalFilename", &exe_name);
    res.set_icon_with_id("images/stremio.ico", "MAINICON");
    res.append_rc_content(r##"SPLASHIMAGE IMAGE "images/stremio.png""##);
    res.compile().unwrap();

    //extract libmpv-2
    let target = std::env::var("TARGET").unwrap();
    let (arch, archive, flags) = match target.as_str() {
        "x86_64-pc-windows-msvc" => ("x64", "libmpv-2_x64.zip", "/LIBPATH:.\\mpv-x64"),
        "aarch64-pc-windows-msvc" => ("arm64", "libmpv-2_arm64.zip", "/LIBPATH:.\\mpv-arm64"),
        _ => panic!("Unsupported target {}", target),
    };
    println!("cargo:rustc-env=ARCH={}", arch);
    println!("cargo:rustc-link-arg={}", flags);
    let archive = fs::read(archive).unwrap();
    let target_dir = PathBuf::from(".");
    zip::ZipArchive::new(Cursor::new(archive))
        .expect("invalid libmpv archive")
        .extract(&target_dir)
        .expect("failed to extract libmpv archive");

    copy_steam_runtime_dll();
}

#[cfg(windows)]
fn copy_steam_runtime_dll() {
    let dll = match find_steam_runtime_dll() {
        Some(path) => path,
        None => {
            println!("cargo:warning=steam_api64.dll was not found; Steam sync binaries may not launch until the DLL is copied next to the executable");
            return;
        }
    };

    let out_dir = match env::var("OUT_DIR").map(PathBuf::from) {
        Ok(path) => path,
        Err(_) => return,
    };
    let profile_dir = match out_dir.ancestors().find(|p| {
        p.file_name()
            .map(|n| n == "debug" || n == "release")
            .unwrap_or(false)
    }) {
        Some(path) => path.to_path_buf(),
        None => return,
    };

    for dir in [profile_dir.clone(), profile_dir.join("deps")] {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::copy(&dll, dir.join("steam_api64.dll"));
    }
}

#[cfg(not(windows))]
fn copy_steam_runtime_dll() {}

#[cfg(windows)]
fn find_steam_runtime_dll() -> Option<PathBuf> {
    if let Ok(sdk) = env::var("STEAM_SDK_LOCATION") {
        let candidate = Path::new(&sdk)
            .join("redistributable_bin")
            .join("win64")
            .join("steam_api64.dll");
        if candidate.exists() {
            return Some(candidate);
        }
    }

    if let Ok(out_dir) = env::var("OUT_DIR") {
        if let Some(target_dir) = Path::new(&out_dir)
            .ancestors()
            .find(|p| p.file_name().map(|n| n == "target").unwrap_or(false))
        {
            if let Some(path) = find_file_named(target_dir, "steam_api64.dll", 8) {
                return Some(path);
            }
        }
    }

    let cargo_home = env::var("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|_| env::var("USERPROFILE").map(|home| Path::new(&home).join(".cargo")))
        .ok()?;
    find_file_named(
        &cargo_home.join("registry").join("src"),
        "steam_api64.dll",
        8,
    )
}

#[cfg(windows)]
fn find_file_named(root: &Path, name: &str, max_depth: usize) -> Option<PathBuf> {
    if max_depth == 0 || !root.is_dir() {
        return None;
    }

    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_file_named(&path, name, max_depth - 1) {
                return Some(found);
            }
        }
    }

    None
}
