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
    pub fn new(current_version: Version, updater_endpoint: &Url, force_update: bool) -> Self {
        Self {
            current_version,
            endpoint: updater_endpoint.clone(),
            force_update,
        }
    }

    pub fn autoupdate(&self) -> Result<Option<Update>, anyhow::Error> {
        println!("Fetching updates for v{}", self.current_version);
        println!("Using updater endpoint {}", self.endpoint);

        let client = reqwest::blocking::Client::builder()
            .user_agent("stremio-shell-ng")
            .build()?;
        let release = client
            .get(self.endpoint.clone())
            .send()?
            .error_for_status()?
            .json::<GitHubRelease>()?;

        let version = Version::parse(release.tag_name.trim_start_matches('v'))?;
        if release.prerelease {
            println!("Skipping GitHub prerelease v{version}");
            return Ok(None);
        }

        if !self.force_update && version <= self.current_version {
            println!("No new releases found newer than v{}", self.current_version);
            return Ok(None);
        }

        let installer = release
            .assets
            .iter()
            .find(|asset| {
                let name = asset.name.to_ascii_lowercase();
                name.ends_with(".exe") && name.contains("setup")
            })
            .or_else(|| {
                release
                    .assets
                    .iter()
                    .find(|asset| asset.name.to_ascii_lowercase().ends_with(".exe"))
            })
            .context("No Windows installer asset found in the latest GitHub release")?;

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
}
