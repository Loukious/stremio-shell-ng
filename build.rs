use chrono::{Datelike, Local};
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const LIBMPV_RSS_URL: &str = "https://sourceforge.net/projects/mpv-player-windows/rss?path=/libmpv";

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

    let target = std::env::var("TARGET").unwrap();
    let (arch, sourceforge_prefix, flags) = match target.as_str() {
        "x86_64-pc-windows-msvc" => ("x64", "mpv-dev-x86_64-v3-", "/LIBPATH:.\\mpv-x64"),
        "aarch64-pc-windows-msvc" => ("arm64", "mpv-dev-aarch64-", "/LIBPATH:.\\mpv-arm64"),
        _ => panic!("Unsupported target {}", target),
    };
    println!("cargo:rustc-env=ARCH={}", arch);
    println!("cargo:rustc-link-arg={}", flags);
    println!("cargo:rerun-if-env-changed=STREMIO_LIBMPV_ARCHIVE");
    println!("cargo:rerun-if-env-changed=STREMIO_LIBMPV_REFRESH");
    println!("cargo:rerun-if-changed=mpv-{arch}/mpv.lib");

    let libmpv_dll = prepare_libmpv(&target, sourceforge_prefix)
        .unwrap_or_else(|err| panic!("failed to prepare libmpv: {}", err));
    copy_runtime_dll(&libmpv_dll, "libmpv-2.dll");

    if env::var_os("CARGO_FEATURE_STEAM_SYNC").is_some() {
        copy_steam_runtime_dll();
    }
}

fn prepare_libmpv(
    target: &str,
    sourceforge_prefix: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cache_dir = cargo_target_dir().join("libmpv").join(target);
    fs::create_dir_all(&cache_dir)?;

    if let Some(archive) = env::var_os("STREMIO_LIBMPV_ARCHIVE").map(PathBuf::from) {
        return extract_libmpv_archive(&archive, &cache_dir);
    }

    match latest_libmpv_download(sourceforge_prefix) {
        Ok(download_url) => {
            let filename = download_url
                .split('/')
                .rev()
                .nth(1)
                .filter(|name| !name.is_empty())
                .ok_or("SourceForge download URL did not contain a filename")?;
            let archive = cache_dir.join(filename);
            if !archive.exists() {
                download_file(&download_url, &archive)?;
            }
            extract_libmpv_archive(&archive, &cache_dir)
        }
        Err(fetch_error) => find_cached_libmpv(&cache_dir).ok_or_else(|| {
            format!("could not discover the latest libmpv ({fetch_error}) and no cached DLL exists")
                .into()
        }),
    }
}

fn latest_libmpv_download(sourceforge_prefix: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut response = ureq::get(LIBMPV_RSS_URL)
        .header("User-Agent", "stremio-shell-ng-build")
        .call()?;
    let rss = response.body_mut().read_to_string()?;
    let document = roxmltree::Document::parse(&rss)?;

    document
        .descendants()
        .filter(|node| node.has_tag_name("item"))
        .find_map(|item| {
            let title = item
                .children()
                .find(|node| node.has_tag_name("title"))
                .and_then(|node| node.text())?;
            let filename = Path::new(title).file_name()?.to_str()?;
            if !filename.starts_with(sourceforge_prefix) {
                return None;
            }

            item.children()
                .find(|node| node.has_tag_name("link"))
                .and_then(|node| node.text())
                .map(str::to_string)
        })
        .ok_or_else(|| {
            format!("SourceForge RSS did not contain a {sourceforge_prefix} package").into()
        })
}

fn download_file(url: &str, destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("downloading latest libmpv package from {url}");
    let temporary = destination.with_extension("7z.part");
    let mut response = ureq::get(url)
        .header("User-Agent", "stremio-shell-ng-build")
        .call()?;
    let mut output = fs::File::create(&temporary)?;
    io::copy(&mut response.body_mut().as_reader(), &mut output)?;
    output.flush()?;
    fs::rename(temporary, destination)?;
    Ok(())
}

fn extract_libmpv_archive(
    archive: &Path,
    cache_dir: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let package_name = archive
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or("libmpv archive has no valid filename")?;
    let extraction_dir = cache_dir.join(package_name);
    let dll = extraction_dir.join("libmpv-2.dll");
    if dll.exists() {
        return Ok(dll);
    }

    if extraction_dir.exists() {
        fs::remove_dir_all(&extraction_dir)?;
    }
    fs::create_dir_all(&extraction_dir)?;
    sevenz_rust2::decompress_file(archive, &extraction_dir)?;

    if !dll.exists() {
        return Err(format!("{} did not contain libmpv-2.dll", archive.display()).into());
    }
    Ok(dll)
}

fn find_cached_libmpv(cache_dir: &Path) -> Option<PathBuf> {
    fs::read_dir(cache_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("libmpv-2.dll"))
        .find(|dll| dll.exists())
}

fn cargo_target_dir() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::current_dir()
                .expect("cannot resolve project directory")
                .join("target")
        })
}

fn copy_runtime_dll(source: &Path, filename: &str) {
    for dir in profile_output_dirs() {
        let _ = fs::create_dir_all(&dir);
        fs::copy(source, dir.join(filename))
            .unwrap_or_else(|err| panic!("failed to copy {}: {err}", source.display()));
    }
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

    copy_runtime_dll(&dll, "steam_api64.dll");
}

#[cfg(not(windows))]
fn copy_steam_runtime_dll() {}

fn profile_output_dirs() -> Vec<PathBuf> {
    let Ok(out_dir) = env::var("OUT_DIR").map(PathBuf::from) else {
        return Vec::new();
    };
    let Some(profile_dir) = out_dir.ancestors().find(|path| {
        path.file_name()
            .map(|name| name == "debug" || name == "release")
            .unwrap_or(false)
    }) else {
        return Vec::new();
    };

    vec![profile_dir.to_path_buf(), profile_dir.join("deps")]
}

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
