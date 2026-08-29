use serde::Deserialize;
use tracing_subscriber::filter::LevelFilter;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RegOpsConfig {
    pub git: GitConfig,
    pub behavior: BehaviorConfig,
    pub system_integration: SystemIntegrationConfig,
}

#[derive(Debug, Deserialize)]
pub struct GitConfig {
    pub repo: Option<String>,
    pub branch: String,
    pub local_path: String,
    pub expected_state_path: String,
}

#[derive(Debug, Deserialize)]
pub struct BehaviorConfig {
    pub mode: RunningMode,
    pub interval_sec: u64,
}

#[derive(Debug, Deserialize)]
pub enum RunningMode {
    Assess,
    Enforce,
}

#[derive(Debug, Deserialize)]
pub struct SystemIntegrationConfig {
    pub log_level: LogLevel,
    pub log_format: LogFormat,
}

#[derive(Debug, Deserialize)]
pub enum LogFormat {
    Raw,
    Json,
}

#[derive(Debug, Deserialize)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn to_tracing_level(&self) -> LevelFilter {
        match self {
            LogLevel::Trace => LevelFilter::TRACE,
            LogLevel::Debug => LevelFilter::DEBUG,
            LogLevel::Info => LevelFilter::INFO,
            LogLevel::Warn => LevelFilter::WARN,
            LogLevel::Error => LevelFilter::ERROR,
        }
    }
}
