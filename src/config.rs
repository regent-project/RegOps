use crate::common::{GitAuthentication, LogFormat, LogLevel, PackageFormat, RunningMode};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct RegOpsConfig {
    pub git: GitConfig,
    pub behavior: BehaviorConfig,
    pub system_integration: SystemIntegrationConfig,
    pub auto_update: Option<AutoUpdateConfig>,
}

impl RegOpsConfig {
    pub fn authentication_mode(&self) -> GitAuthentication {
        if let Some(authentication_config) = &self.git.auth {
            if let Some(token_value) = &authentication_config.token {
                return GitAuthentication::WithToken(token_value.clone());
            }
        }
        GitAuthentication::None
    }

    /// Validate cross-section constraints.
    /// Returns Err with a human-readable message if the config is inconsistent.
    pub fn validate(&self) -> Result<(), String> {
        if self.behavior.auto_update && self.auto_update.is_none() {
            return Err(
                "behavior.auto_update is true but no [AutoUpdate] section is present".to_string(),
            );
        }
        if let Some(auto_update) = &self.auto_update {
            if auto_update.github_api_url.trim().is_empty() {
                return Err("auto_update.github_api_url must not be empty".to_string());
            }
            if auto_update.check_interval_sec == 0 {
                return Err("auto_update.check_interval_sec must be greater than 0".to_string());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GitConfig {
    pub repo: Option<String>,
    pub branch: String,
    pub local_path: String,
    pub expected_state_path: String,
    pub auth: Option<AuthMode>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AuthMode {
    pub token: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BehaviorConfig {
    pub mode: RunningMode,
    pub interval_sec: u64,
    #[serde(default)]
    pub auto_update: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SystemIntegrationConfig {
    pub log_level: LogLevel,
    pub log_format: LogFormat,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AutoUpdateConfig {
    /// GitHub API endpoint for the latest release, e.g.
    /// "https://api.github.com/repos/regent-project/regops/releases/latest"
    pub github_api_url: String,
    /// How often to check for a new release, in seconds.
    pub check_interval_sec: u64,
    /// Package format to download and install.
    #[serde(default)]
    pub package_format: PackageFormat,
}

pub fn load_config(path: &str) -> Result<RegOpsConfig, String> {
    let mut configuration_file = match File::open(path) {
        Ok(file) => file,
        Err(details) => return Err(format!("Failed to open '{}': {}", path, details)),
    };

    let mut file_content: Vec<u8> = Vec::new();
    match configuration_file.read_to_end(&mut file_content) {
        Ok(_size) => {}
        Err(details) => return Err(format!("Failed to read '{}': {}", path, details)),
    }

    match toml::from_slice::<RegOpsConfig>(&file_content) {
        Ok(config) => match config.validate() {
            Ok(()) => Ok(config),
            Err(details) => Err(format!("Invalid configuration in '{}': {}", path, details)),
        },
        Err(details) => Err(format!("Failed to parse '{}': {}", path, details)),
    }
}
