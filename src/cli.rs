use clap::{Parser, Subcommand};
use tracing::{error, info, warn};

use crate::common::{LogFormat, LogLevel, RunningMode, ensure_directory_exists};
use crate::config::{
    AuthMode, BehaviorConfig, GitConfig, RegOpsConfig, SystemIntegrationConfig, load_config,
};
use std::os::unix::fs::PermissionsExt;

#[derive(Parser, Debug)]
#[command(name = "regops")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "GitOps agent for localhost configuration management", long_about = None)]
pub struct Cli {
    #[arg(short, long, global = true, default_value = "/etc/regops/config.toml")]
    pub config_path: String,

    /// No subcommand: run the service (matches the shipped systemd unit).
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    Run,
    Config {
        #[command(subcommand)]
        config_command: ConfigCommands,
    },
    RunOnce,
    /// Generate shell completion scripts
    #[command(subcommand)]
    Completion(CompletionCommands),
}

#[derive(Subcommand, Debug)]
pub enum CompletionCommands {
    Bash,
    Zsh,
    Fish,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommands {
    Get,
    Init {
        #[arg(long)]
        repo: String,
        #[arg(short, long, default_value = "main")]
        branch: String,
        #[arg(long, default_value = "/home/regops/repo")]
        local_path: String,
        #[arg(long, default_value = "expected_state.yaml")]
        expected_state_path: String,
        #[arg(short, long, value_enum, default_value = "assess")]
        mode: RunningMode,
        #[arg(short, long, default_value = "30")]
        interval_sec: u64,
        #[arg(long, value_enum, default_value = "info")]
        log_level: LogLevel,
        #[arg(long, value_enum, default_value = "raw")]
        log_format: LogFormat,
        /// Name of the environment variable holding the Git access token.
        /// The token value itself is never accepted on the command line, so
        /// it cannot appear in argv or shell history.
        #[arg(long, default_value = "REGOPS_GIT_TOKEN")]
        token_env_var_name: String,
    },
    Validate,
}

pub fn config_get(config_path: &str) {
    match load_config(config_path) {
        Ok(config) => {
            let toml_string = toml::to_string(&config).unwrap_or_else(|e| {
                error!("Failed to serialize config: {}", e);
                std::process::exit(1);
            });
            info!("{}", toml_string);
        }
        Err(e) => {
            error!("{}", e);
            std::process::exit(1);
        }
    }
}

pub fn config_init(
    config_path: &str,
    repo: &str,
    branch: &str,
    local_path: &str,
    expected_state_path: &str,
    mode: RunningMode,
    interval_sec: u64,
    log_level: LogLevel,
    log_format: LogFormat,
    token_env_var_name: &str,
) {
    let token = match std::env::var(token_env_var_name) {
        Ok(token) => {
            if token.is_empty() {
                error!(
                    "Environment variable '{token_env_var_name}' is set but empty. Give it the token value or unset it to configure no authentication"
                );
                std::process::exit(1);
            }
            Some(token)
        }
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(details)) => {
            error!("Environment variable '{token_env_var_name}' is not valid unicode: {details:?}");
            std::process::exit(1);
        }
    };
    let auth = token.clone().map(|token| AuthMode { token: Some(token) });
    let config = RegOpsConfig {
        git: GitConfig {
            repo: Some(repo.to_string()),
            branch: branch.to_string(),
            local_path: local_path.to_string(),
            expected_state_path: expected_state_path.to_string(),
            auth,
        },
        behavior: BehaviorConfig { mode, interval_sec },
        system_integration: SystemIntegrationConfig {
            log_level,
            log_format,
        },
    };

    let toml_string = toml::to_string(&config).unwrap_or_else(|e| {
        error!("Failed to serialize config: {}", e);
        std::process::exit(1);
    });

    ensure_directory_exists(config_path);

    if let Err(e) = std::fs::write(config_path, toml_string) {
        error!("Failed to write config file: {}", e);
        std::process::exit(1);
    }

    // The config now holds a secret: readable by the regops service user only.
    if token.is_some() {
        if let Err(details) =
            std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o640))
        {
            warn!(%details, "Failed to restrict config file permissions");
        }
        match std::process::Command::new("chown")
            .args(["regops:regops", config_path])
            .status()
        {
            Ok(status) if status.success() => {}
            details => warn!(?details, "Failed to chown config file to regops"),
        }
    }

    info!("Configuration file initialized at: {}", config_path);
}

pub fn config_validate(config_path: &str) {
    match load_config(config_path) {
        Ok(config) => {
            if config.git.repo.is_none() {
                warn!("No git repository configured (git.repo is unset)");
            }

            info!("Configuration file is valid");
        }
        Err(e) => {
            error!("Invalid configuration file: {}", e);
            std::process::exit(1);
        }
    }
}
