use clap::{CommandFactory, Parser};
use regent_sdk::hosts::handlers::{ConnectionMethod, TargetUser};
use regent_sdk::hosts::managed_host::{ManagedHost, ManagedHostBuilder};
use tokio::time::{Duration, sleep};
use tracing::{error, info, span, warn};
use tracing_subscriber::{Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

mod cli;
mod common;
mod config;
mod git;
mod run;

use crate::cli::{Cli, Commands, CompletionCommands, ConfigCommands};
use crate::common::init_tracing;
use crate::config::load_config;
use crate::run::perform_compliance_pass;
use tracing_subscriber::filter::LevelFilter;

fn init_default_tracing() {
    let fmt_layer = fmt::layer().boxed();

    tracing_subscriber::registry()
        .with(LevelFilter::INFO)
        .with(fmt_layer)
        .try_init()
        .ok();
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Initialize default tracing for CLI commands
    init_default_tracing();

    // Handle CLI commands that don't run the service
    match cli.command {
        Commands::Run => {
            service_main(&cli.config_path).await;
        }
        Commands::Config { config_command } => {
            match config_command {
                ConfigCommands::Get => {
                    cli::config_get(&cli.config_path);
                }
                ConfigCommands::Init {
                    repo,
                    branch,
                    local_path,
                    expected_state_path,
                    mode,
                    interval_sec,
                    log_level,
                    log_format,
                } => {
                    cli::config_init(
                        &cli.config_path,
                        &repo,
                        &branch,
                        &local_path,
                        &expected_state_path,
                        mode,
                        interval_sec,
                        log_level,
                        log_format,
                    );
                }
                ConfigCommands::Validate => {
                    cli::config_validate(&cli.config_path);
                }
            }
            std::process::exit(0);
        }
        Commands::RunOnce => {
            if let Err(details) = run::run_once(&cli.config_path).await {
                error!("On-demand pass failed: {}", details);
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        Commands::Completion(completion_command) => {
            use clap_complete::{Shell, generate};
            use std::io;

            let mut app = cli::Cli::command();
            let shell = match completion_command {
                CompletionCommands::Bash => Shell::Bash,
                CompletionCommands::Zsh => Shell::Zsh,
                CompletionCommands::Fish => Shell::Fish,
            };
            generate(shell, &mut app, "regops", &mut io::stdout());
            std::process::exit(0);
        }
    }
}

async fn service_main(config_path: &str) {
    loop {
        if let Err(details) = run(config_path).await {
            error!("unrecoverable error, retrying shortly: {}", details);
        }
        sleep(Duration::from_secs(10)).await;
    }
}

async fn run(config_path: &str) -> Result<(), String> {
    let config = match load_config(config_path) {
        Ok(config) => config,
        Err(details) => return Err(format!("Failed to load configuration: {}", details)),
    };

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

    // Regent initialization.
    // We expect the user which runs Regops to have required permissions with
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
        if let Err(details) =
            perform_compliance_pass(&mut managed_localhost, &config, &auth, &repo).await
        {
            error!(
                details,
                "Failed to perform compliance pass, will retry next cycle"
            );
        }
        sleep(Duration::from_secs(config.behavior.interval_sec)).await;
    }
}
