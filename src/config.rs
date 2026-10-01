use crate::common::{GitAuthentication, LogFormat, LogLevel, RunningMode};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct RegOpsConfig {
    pub git: GitConfig,
    pub behavior: BehaviorConfig,
    pub system_integration: SystemIntegrationConfig,
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
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SystemIntegrationConfig {
    pub log_level: LogLevel,
    pub log_format: LogFormat,
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

    match toml::from_slice(&file_content) {
        Ok(config) => Ok(config),
        Err(details) => Err(format!("Failed to parse '{}': {}", path, details)),
    }
}
