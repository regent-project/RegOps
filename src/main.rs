use std::fs::File;
use std::io::Read;
use tokio::time::{Duration, sleep};
use tracing::{error, info, span, warn};
use tracing_subscriber::{Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};
use regent_sdk::hosts::managed_host::{ManagedHost, ManagedHostBuilder};
use regent_sdk::hosts::handlers::{ConnectionMethod, TargetUser};
use regent_sdk::ExpectedState;

mod config;
mod git;

use crate::config::{LogFormat, RegOpsConfig, RunningMode, SystemIntegrationConfig};
use crate::git::{clone_fresh, git_pull, local_repo_matches_expected};

#[tokio::main]
async fn main() {
    // run() only returns once its own operational loop is interrupted by an
    // unrecoverable startup error (bad config, no repository configured yet,
    // failed clone, failed connection to the managed host...).
    loop {
        if let Err(details) = run().await {
            eprintln!("[FATAL] unrecoverable error, retrying shortly: {}", details);
        }
        sleep(Duration::from_secs(10)).await;
    }
}

async fn run() -> Result<(), String> {
    let config = match load_config("/etc/regops/config.toml") {
        Ok(config) => config,
        Err(details) => return Err(format!("Failed to load configuration: {}", details)),
    };

    // Tracing may already be initialized from a previous retry loop iteration
    // We just keep using the existing subscriber.
    init_tracing(&config.system_integration);

    // Get hostname first for the global tracing span and for the managed host id
    let hostname = match hostname::get() {
        Ok(value) => value.to_string_lossy().to_string(),
        Err(details) => {
            warn!(%details, "Failed to get hostname");
            "localhost".to_string()
        }
    };
    let global_span = span!(tracing::Level::INFO, "RegOps", ?hostname);
    let _guard = global_span.enter();

    // Right after installation, the repository might not be configured yet.
    // Treat that as a startup error like any other: it gets logged and
    // retried, and re-reading the config file on each retry means a
    // repository added later is picked up without a restart.
    let repo = match &config.git.repo {
        Some(repo) => repo.clone(),
        None => {
            warn!("No repository url set yet");
            return Err("No git repository configured (git.repo is unset)".to_string());
        }
    };

    info!("Git repository : {}", repo);
    info!("Running mode : {:?}", config.behavior.mode);

    let auth = config.authentication_mode();

    // Check whether the local copy already present matches what's expected
    // (valid repository, right remote, right branch checked out). Any
    // mismatch or issue (missing folder, wrong branch, merge conflict, stale
    // remote...) is not worth diagnosing: wipe it and clone fresh.
    if !local_repo_matches_expected(&config.git.local_path, &repo, &config.git.branch) {
        match clone_fresh(&config.git.local_path, &repo, &config.git.branch, &auth) {
            Ok(()) => {}
            Err(details) => return Err(format!("Failed initial cloning: {}", details)),
        }
    }

    // Regent initialization.
    // We expect the user which runs RegOps to have required permissions with
    // non-interactive sudo capability.
    let managed_host_builder = ManagedHostBuilder::new(
        &hostname,
        "localhost",
        Some(ConnectionMethod::Localhost(TargetUser::current_user())),
    );

    let mut managed_localhost: ManagedHost = match managed_host_builder.build(None).await {
        Ok(managed_host) => managed_host,
        Err(details) => return Err(format!("Failed to build managed host: {}", details)),
    };

    match managed_localhost.connect().await {
        Ok(()) => {}
        Err(details) => return Err(format!("Failed to connect to managed host: {}", details)),
    }

    // Operational loop
    loop {
        
        match git_pull(&config.git.local_path, &auth) {
            Ok(()) => {}
            Err(details) => {
                warn!(details, "Failed to pull git repository, wiping local copy and re-cloning");
                match clone_fresh(&config.git.local_path, &repo, &config.git.branch, &auth) {
                    Ok(()) => {}
                    Err(recovery_details) => {
                        error!(recovery_details, "Failed to recover local git repository, will retry next cycle");
                    }
                }
                sleep(Duration::from_secs(config.behavior.interval_sec)).await;
                continue;
            }
        }

        // Regent part
        let expected_state_description = match std::fs::read_to_string(format!(
            "{}/{}",
            config.git.local_path, config.git.expected_state_path
        )) {
            Ok(content) => content,
            Err(details) => {
                error!(?details, "Failed to get file content");
                sleep(Duration::from_secs(config.behavior.interval_sec)).await;
                continue;
            }
        };

        let expected_state = match ExpectedState::from_raw_yaml(&expected_state_description) {
            Ok(state) => state,
            Err(error_detail) => {
                error!("Wrong yaml content : {:?}", error_detail);
                sleep(Duration::from_secs(config.behavior.interval_sec)).await;
                continue;
            }
        };

        match &config.behavior.mode {
            RunningMode::Assess => {
                match managed_localhost.assess_compliance(&expected_state, true).await {
                    Ok(_assessment) => {}
                    Err(details) => warn!(%details, "Failed to assess compliance"),
                }
            }
            RunningMode::Enforce => {
                match managed_localhost.reach_compliance(&expected_state).await {
                    Ok(_outcome) => {}
                    Err(details) => warn!(%details, "Failed to enforce compliance"),
                }
            }
        }

        sleep(Duration::from_secs(config.behavior.interval_sec)).await;
    }
}

fn load_config(path: &str) -> Result<RegOpsConfig, String> {
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

fn init_tracing(system_integration: &SystemIntegrationConfig) {
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
