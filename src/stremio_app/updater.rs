use std::{
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{anyhow, Context};
use semver::Version;
use serde::Deserialize;
use url::Url;

#[derive(Debug, Clone)]
pub struct Update {
    pub version: Version,
    pub file: PathBuf,
}

#[derive(Debug)]
pub struct Updater {
    pub current_version: Version,
    pub endpoint: Url,
    pub force_update: bool,
    pub include_prerelease: bool,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: Url,
}

impl Updater {
    pub fn new(
        current_version: Version,
        updater_endpoint: &Url,
        force_update: bool,
        include_prerelease: bool,
    ) -> Self {
        Self {
            current_version,
            endpoint: updater_endpoint.clone(),
            force_update,
            include_prerelease,
        }
    }

    fn target_arch_suffix() -> &'static str {
        if cfg!(target_arch = "aarch64") {
            "_arm64"
        } else {
            "_x64"
        }
    }

    pub fn autoupdate(&self) -> Result<Option<Update>, anyhow::Error> {
        println!("Fetching updates for v{}", self.current_version);
        println!("Using updater endpoint {}", self.endpoint);

        let client = reqwest::blocking::Client::builder()
            .user_agent("stremio-shell-ng")
            .build()?;
        let release = self.fetch_release(&client)?;

        let version = Version::parse(release.tag_name.trim_start_matches('v'))?;
        if release.prerelease && !self.include_prerelease {
            println!("Skipping GitHub prerelease v{version}");
            return Ok(None);
        }

        if !self.force_update && version <= self.current_version {
            println!("No new releases found newer than v{}", self.current_version);
            return Ok(None);
        }

        let arch_suffix = Self::target_arch_suffix();
        let installer = release
            .assets
            .iter()
            .find(|asset| {
                let name = asset.name.to_ascii_lowercase();
                name.ends_with(".exe")
                    && name.contains("setup")
                    && name.contains(arch_suffix)
            })
            .or_else(|| {
                release
                    .assets
                    .iter()
                    .find(|asset| {
                        let name = asset.name.to_ascii_lowercase();
                        name.ends_with(".exe") && name.contains(arch_suffix)
                    })
            })
            .context(format!(
                "No Windows installer asset found for architecture suffix {arch_suffix}"
            ))?;

        let dest = std::env::temp_dir().join(&installer.name);
        println!(
            "Downloading {} to {}",
            installer.browser_download_url,
            dest.display()
        );

        let mut installer_response = client
            .get(installer.browser_download_url.clone())
            .send()?
            .error_for_status()?;
        let size = installer_response.content_length();
        let mut downloaded = 0u64;
        let mut chunk = [0u8; 8192];
        let mut file = std::fs::File::create(&dest)?;

        loop {
            let chunk_size = installer_response.read(&mut chunk)?;
            if chunk_size == 0 {
                break;
            }
            file.write_all(&chunk[..chunk_size])?;
            downloaded += chunk_size as u64;
            if let Some(size) = size {
                print!("\rProgress: {}%", downloaded * 100 / size);
            } else {
                print!(".");
            }
            std::io::stdout().flush().ok();
        }
        println!();

        if downloaded == 0 {
            std::fs::remove_file(&dest).ok();
            return Err(anyhow!("Installer download was empty"));
        }
        if let Some(size) = size {
            if downloaded != size {
                std::fs::remove_file(&dest).ok();
                return Err(anyhow!(
                    "Incomplete installer download: expected {size} bytes, received {downloaded}"
                ));
            }
        }

        Ok(Some(Update {
            version,
            file: dest,
        }))
    }

    fn fetch_release(
        &self,
        client: &reqwest::blocking::Client,
    ) -> Result<GitHubRelease, anyhow::Error> {
        if self.include_prerelease && self.endpoint.path().ends_with("/releases/latest") {
            let mut releases_url = self.endpoint.clone();
            let releases_path = releases_url
                .path()
                .strip_suffix("/latest")
                .expect("checked suffix")
                .to_string();
            releases_url.set_path(&releases_path);
            let releases = client
                .get(releases_url)
                .send()?
                .error_for_status()?
                .json::<Vec<GitHubRelease>>()?;

            return releases
                .into_iter()
                .filter_map(|release| {
                    let version = Version::parse(release.tag_name.trim_start_matches('v')).ok()?;
                    Some((version, release))
                })
                .max_by(|(left, _), (right, _)| left.cmp(right))
                .map(|(_, release)| release)
                .context("No valid releases found in the GitHub response");
        }

        Ok(client
            .get(self.endpoint.clone())
            .send()?
            .error_for_status()?
            .json::<GitHubRelease>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn newer(current: &str, candidate: &str) -> bool {
        Version::parse(candidate).unwrap() > Version::parse(current).unwrap()
    }

    #[test]
    fn rc_versions_are_accepted_by_semver_ordering() {
        assert!(newer("5.0.20", "5.0.21-rc2"));
        assert!(newer("5.0.21-rc1", "5.0.21-rc2"));
        assert!(newer("5.0.21-rc2", "5.0.21"));
    }

    #[test]
    fn final_release_does_not_update_to_older_rc() {
        assert!(!newer("5.0.21", "5.0.21-rc2"));
    }

    #[test]
    fn prefers_arch_matching_installer_asset() {
        let assets = vec![
            GitHubAsset {
                name: "StremioSetup-v1.2.3_x64.exe".to_string(),
                browser_download_url: Url::parse("https://example.com/x64.exe").unwrap(),
            },
            GitHubAsset {
                name: "StremioSetup-v1.2.3_arm64.exe".to_string(),
                browser_download_url: Url::parse("https://example.com/arm64.exe").unwrap(),
            },
        ];

        let selected = assets
            .iter()
            .find(|asset| {
                let name = asset.name.to_ascii_lowercase();
                name.ends_with(".exe")
                    && name.contains("setup")
                    && name.contains(Updater::target_arch_suffix())
            })
            .expect("matching installer should be selected");

        if cfg!(target_arch = "aarch64") {
            assert!(selected.name.ends_with("_arm64.exe"));
        } else {
            assert!(selected.name.ends_with("_x64.exe"));
        }
    }
}
