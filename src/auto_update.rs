use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use semver::Version;
use serde::Deserialize;
use tracing::{info, warn};

use crate::common::PackageFormat;
use crate::config::AutoUpdateConfig;

/// Outcome of a self-update attempt.
pub enum UpdateOutcome {
    /// No update available; the current version is up to date.
    UpToDate,
    /// A new version was downloaded and installed. The caller should exit
    /// so systemd restarts the service on the new binary.
    Updated,
}

/// Subset of the GitHub "latest release" API response that we need.
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

/// Parse a version string like "v0.10.1" or "0.10.1" into a semver::Version.
fn parse_version(raw: &str) -> Option<Version> {
    let trimmed = raw.trim().trim_start_matches('v').trim_start_matches('V');
    Version::parse(trimmed).ok()
}

/// Determine the current binary's version from CARGO_PKG_VERSION.
fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is always valid semver")
}

/// Detect which package format to use on this host.
fn detect_package_format(requested: PackageFormat) -> &'static str {
    match requested {
        PackageFormat::Deb => "deb",
        PackageFormat::Rpm => "rpm",
        PackageFormat::Auto => {
            if Path::new("/usr/bin/dpkg").exists() {
                "deb"
            } else if Path::new("/usr/bin/rpm").exists() || Path::new("/bin/rpm").exists() {
                "rpm"
            } else {
                // Fallback: assume Debian-family (most common target).
                warn!("Could not detect package manager (dpkg/rpm); defaulting to .deb");
                "deb"
            }
        }
    }
}

/// Query the GitHub API for the latest release and return the release info.
async fn fetch_latest_release(
    client: &reqwest::Client,
    api_url: &str,
) -> Result<GitHubRelease, String> {
    let response = client
        .get(api_url)
        .header("User-Agent", concat!("regops/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("Failed to query GitHub API: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "GitHub API returned HTTP {} for {}",
            response.status(),
            api_url
        ));
    }

    response
        .json::<GitHubRelease>()
        .await
        .map_err(|e| format!("Failed to parse GitHub API response: {}", e))
}

/// Pick the right asset from a release based on the desired package format.
fn select_asset<'a>(release: &'a GitHubRelease, format: &str) -> Option<&'a GitHubAsset> {
    let suffix = format!(".{}", format);
    release
        .assets
        .iter()
        .find(|asset| asset.name.ends_with(&suffix))
}

/// Download a release asset to a temporary file. Returns the temp file path.
async fn download_asset(
    client: &reqwest::Client,
    asset: &GitHubAsset,
    format: &str,
) -> Result<String, String> {
    let response = client
        .get(&asset.browser_download_url)
        .header("User-Agent", concat!("regops/", env!("CARGO_PKG_VERSION")))
        .send()
        .await
        .map_err(|e| format!("Failed to download asset '{}': {}", asset.name, e))?;

    if !response.status().is_success() {
        return Err(format!(
            "Failed to download asset '{}': HTTP {}",
            asset.name,
            response.status()
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read download body for '{}': {}", asset.name, e))?;

    let temp_path = std::env::temp_dir().join(format!("regops-update.{}", format));
    let temp_path_str = temp_path.to_string_lossy().to_string();

    std::fs::write(&temp_path, &bytes)
        .map_err(|e| format!("Failed to write temp file '{}': {}", temp_path_str, e))?;

    info!(asset = %asset.name, path = %temp_path_str, "Downloaded release asset");
    Ok(temp_path_str)
}

/// Install a downloaded package using the system package manager (via sudo).
fn install_package(package_path: &str, format: &str) -> Result<(), String> {
    let (program, args): (&str, Vec<&str>) = match format {
        "deb" => ("sudo", vec!["dpkg", "-i", package_path]),
        "rpm" => ("sudo", vec!["rpm", "-U", package_path]),
        other => return Err(format!("Unsupported package format: {}", other)),
    };

    info!(package = %package_path, format, "Installing package");

    let output = std::process::Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run '{}': {}", program, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "Package installation failed (exit {}): {}\n{}",
            output.status,
            stdout.trim(),
            stderr.trim()
        ));
    }

    info!(format, "Package installed successfully");
    Ok(())
}

/// Check for and install a self-update if one is available.
///
/// Returns `UpdateOutcome::Updated` if a new version was installed (caller
/// should exit so systemd restarts on the new binary), or
/// `UpdateOutcome::UpToDate` if the current version is already the latest.
pub async fn check_and_apply_update(config: &AutoUpdateConfig) -> Result<UpdateOutcome, String> {
    let current = current_version();
    info!(current = %current, "Checking for updates");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let release = fetch_latest_release(&client, &config.github_api_url).await?;

    let latest = match parse_version(&release.tag_name) {
        Some(v) => v,
        None => {
            return Err(format!(
                "Could not parse release tag '{}' as a version",
                release.tag_name
            ));
        }
    };

    info!(latest = %latest, current = %current, "Latest release version");

    if latest <= current {
        info!("Already running the latest version");
        return Ok(UpdateOutcome::UpToDate);
    }

    info!(
        current = %current,
        latest = %latest,
        "New version available, starting self-update"
    );

    let format = detect_package_format(config.package_format);
    let asset = match select_asset(&release, format) {
        Some(asset) => asset,
        None => {
            return Err(format!(
                "No .{} asset found in release '{}'. Available assets: {}",
                format,
                release.tag_name,
                release
                    .assets
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    };

    let package_path = download_asset(&client, asset, format).await?;

    match install_package(&package_path, format) {
        Ok(()) => {}
        Err(details) => {
            // Best-effort cleanup of the downloaded package.
            let _ = std::fs::remove_file(&package_path);
            return Err(details);
        }
    }

    // Clean up the temp file (the package manager has its own copy).
    let _ = std::fs::remove_file(&package_path);

    info!(latest = %latest, "Self-update installed; exiting for restart");
    Ok(UpdateOutcome::Updated)
}

/// A stateful tracker that only checks for updates at the configured interval.
pub struct UpdateChecker {
    config: AutoUpdateConfig,
    last_check: Option<Instant>,
}

impl UpdateChecker {
    pub fn new(config: AutoUpdateConfig) -> Self {
        Self {
            config,
            last_check: None,
        }
    }

    /// Returns true if enough time has elapsed since the last check (or if
    /// this is the first call).
    pub fn should_check(&self) -> bool {
        match self.last_check {
            None => true,
            Some(last) => last.elapsed() >= Duration::from_secs(self.config.check_interval_sec),
        }
    }

    /// Run a check if the interval has elapsed. Updates `last_check` on both
    /// success and failure so we don't hammer the API on repeated errors.
    pub async fn maybe_check(&mut self) -> Option<Result<UpdateOutcome, String>> {
        if !self.should_check() {
            return None;
        }
        self.last_check = Some(Instant::now());
        Some(check_and_apply_update(&self.config).await)
    }
}
