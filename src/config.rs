use crate::common::{GitAuthentication, LogFormat, LogLevel, PackageFormat, Reporting, RunningMode, TlsMode};
use regent_sdk::HostLockSettings;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::time::Duration;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct RegOpsConfig {
    /// Where outcomes go: nowhere (standalone, default) or to regency. A top-level
    /// key: in the TOML file it must come before the first `[Section]`.
    #[serde(default)]
    pub reporting: Reporting,
    pub git: GitConfig,
    pub behavior: BehaviorConfig,
    pub system_integration: SystemIntegrationConfig,
    pub auto_update: Option<AutoUpdateConfig>,
    #[serde(default)]
    pub host_lock: HostLockConfig,
    /// Required when `Reporting = "Regency"`.
    pub regency: Option<RegencyConfig>,
}

impl RegOpsConfig {
    /// Host lock settings for the regent-sdk managed host (`None`: locking disabled).
    pub fn host_lock_settings(&self) -> Option<HostLockSettings> {
        if !self.host_lock.enabled {
            return None;
        }
        let mut settings = HostLockSettings::default();
        if let Some(path) = &self.host_lock.path {
            settings.path = path.clone();
        }
        if let Some(seconds) = self.host_lock.shared_timeout_sec {
            settings.shared_timeout = Duration::from_secs(seconds);
        }
        if let Some(seconds) = self.host_lock.exclusive_timeout_sec {
            settings.exclusive_timeout = Duration::from_secs(seconds);
        }
        Some(settings)
    }

    /// Id of this host for regent-sdk and regency: `[Regency] host_id`, else the
    /// hostname. It must equal the host's id in regency inventories when the host is
    /// also managed by remote runners.
    pub fn host_id(&self, hostname: &str) -> String {
        self.regency
            .as_ref()
            .and_then(|regency| regency.host_id.clone())
            .unwrap_or_else(|| hostname.to_string())
    }

    /// The `[Regency]` section when reporting to regency is enabled.
    pub fn regency_reporting(&self) -> Option<&RegencyConfig> {
        match self.reporting {
            Reporting::None => None,
            Reporting::Regency => self.regency.as_ref(),
        }
    }

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
        if self.reporting == Reporting::Regency {
            let Some(regency) = &self.regency else {
                return Err("Reporting is \"Regency\" but no [Regency] section is present".to_string());
            };
            regency.validate()?;
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

/// Host lock shared with every other Regent process working on this host (e.g.
/// remote runners): assessments run in parallel, a remediation runs alone.
/// Unset fields take regent-sdk's defaults; the path must match the other processes'.
#[derive(Debug, Deserialize, Serialize)]
pub struct HostLockConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub path: Option<String>,
    pub shared_timeout_sec: Option<u64>,
    pub exclusive_timeout_sec: Option<u64>,
}

impl Default for HostLockConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            shared_timeout_sec: None,
            exclusive_timeout_sec: None,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Reporting outcomes to regency's endpoint (gRPC over mTLS).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegencyConfig {
    /// Endpoint URL, e.g. "https://regency.example.com:8443".
    pub endpoint: String,
    /// Host id; default: the hostname.
    pub host_id: Option<String>,
    /// TLS name expected in the endpoint's certificate; default: the URL's host.
    pub server_name: Option<String>,
    /// CA certificate(s), PEM, that signed the endpoint's server certificate.
    pub ca_cert_path: String,
    /// How this host gets its client certificate.
    #[serde(default)]
    pub tls_mode: TlsMode,
    /// Client certificate (PEM). ExternalCa: provided by your PKI; BuiltInCa and
    /// SelfSigned: written by regops.
    #[serde(default = "default_cert_path")]
    pub cert_path: String,
    /// Client private key (PEM), same rules as `cert_path`.
    #[serde(default = "default_key_path")]
    pub key_path: String,
    /// BuiltInCa: file holding the enrollment token generated by a regency admin,
    /// used once to obtain the first certificate.
    pub enrollment_token_path: Option<String>,
    /// BuiltInCa: renew the certificate when it expires within this delay.
    #[serde(default = "default_renew_before_sec")]
    pub renew_before_sec: u64,
    /// Outcomes are kept here until the endpoint accepted them.
    #[serde(default = "default_spool_dir")]
    pub spool_dir: String,
    /// Bound of the spool: beyond it, the oldest unsent outcomes are dropped.
    #[serde(default = "default_spool_max_entries")]
    pub spool_max_entries: usize,
    /// Delay before retrying after the endpoint could not be reached.
    #[serde(default = "default_retry_interval_sec")]
    pub retry_interval_sec: u64,
    /// Timeout of one call to the endpoint.
    #[serde(default = "default_request_timeout_sec")]
    pub request_timeout_sec: u64,
}

impl RegencyConfig {
    fn validate(&self) -> Result<(), String> {
        if !self.endpoint.starts_with("https://") {
            return Err("regency.endpoint must be an https:// URL".to_string());
        }
        if self.ca_cert_path.trim().is_empty() {
            return Err("regency.ca_cert_path must not be empty".to_string());
        }
        if self.host_id.as_deref().is_some_and(|id| id.trim().is_empty()) {
            return Err("regency.host_id must not be empty when set".to_string());
        }
        if self.spool_max_entries == 0 {
            return Err("regency.spool_max_entries must be greater than 0".to_string());
        }
        if self.retry_interval_sec == 0 || self.request_timeout_sec == 0 {
            return Err(
                "regency.retry_interval_sec and regency.request_timeout_sec must be greater than 0"
                    .to_string(),
            );
        }
        Ok(())
    }
}

fn default_cert_path() -> String {
    "/var/lib/regops/regency/client.pem".to_string()
}

fn default_key_path() -> String {
    "/var/lib/regops/regency/client.key".to_string()
}

fn default_renew_before_sec() -> u64 {
    7 * 24 * 3600
}

fn default_spool_dir() -> String {
    "/var/lib/regops/spool".to_string()
}

fn default_spool_max_entries() -> usize {
    1000
}

fn default_retry_interval_sec() -> u64 {
    30
}

fn default_request_timeout_sec() -> u64 {
    10
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
