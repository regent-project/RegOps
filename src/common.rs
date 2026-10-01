use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::error;
use tracing::warn;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::SystemIntegrationConfig;

pub fn ensure_directory_exists(path: &str) {
    let path_obj = Path::new(path);
    if let Some(parent) = path_obj.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                error!("Failed to create directory {}: {}", parent.display(), e);
                std::process::exit(1);
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum LogFormat {
    Raw,
    Json,
}

#[derive(Debug, Deserialize, Serialize, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
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

#[derive(Debug, Deserialize, Serialize, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum RunningMode {
    Assess,
    Enforce,
}

pub enum GitAuthentication {
    None,
    WithToken(String),
}

pub fn init_tracing(system_integration: &SystemIntegrationConfig) {
    let fmt_layer = match system_integration.log_format {
        LogFormat::Raw => fmt::layer().boxed(),
        LogFormat::Json => fmt::layer().json().boxed(),
    };

    match tracing_subscriber::registry()
        .with(system_integration.log_level.to_tracing_level())
        .with(fmt_layer)
        .try_init()
    {
        Ok(()) => {}
        Err(details) => {
            warn!(%details, "Tracing global subscriber init failed");
            // A global subscriber is already installed from a previous retry
            // of run(); keep using it.
        }
    }
}
