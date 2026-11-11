use clap::{CommandFactory, Parser};
use tokio::time::{Duration, sleep};
use tracing::{Instrument, error, info, span, warn};
use tracing_subscriber::{Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

mod auto_update;
mod cli;
mod common;
mod config;
mod git;
mod reporting;
mod run;
#[cfg(windows)]
mod winservice;

use crate::auto_update::{UpdateOutcome, check_and_apply_update};
use crate::cli::{Cli, Commands, CompletionCommands, ConfigCommands};
use crate::common::init_tracing;
use crate::config::{AutoUpdateConfig, load_config};
use crate::reporting::Reporter;
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

/// Log an error through tracing when a subscriber is installed, otherwise
/// fall back to stderr so the message is never silently dropped (e.g. when
/// the config file itself failed to load, before init_tracing() ran).
fn report_error(message: &str) {
    if tracing::dispatcher::has_been_set() {
        error!("{}", message);
    } else {
        eprintln!("{}", message);
    }
}

fn main() {
    // Windows: under the Service Control Manager (MSI service registration)
    #[cfg(windows)]
    if std::env::args().any(|arg| arg == "--windows-service") {
        winservice::run();
    }
    tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async_main());
}

pub(crate) async fn async_main() {
    // Several rustls crypto providers are linked (reqwest, gix, tonic): pick one
    // explicitly, or building a TLS configuration panics.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cli = Cli::parse();

    // Initialize default tracing for CLI commands that never load the
    // config file. The service paths (no subcommand, `run`, `run-once`)
    // must NOT install it here: a global subscriber can only be set once,
    // and init_tracing() installs it from the config (log_format /
    // log_level). Installing a raw default first would silently override
    // the configured log format.
    match &cli.command {
        Some(Commands::Config { .. }) | Some(Commands::Completion(_)) => init_default_tracing(),
        _ => {}
    }

    info!("RegOps starting");

    // Handle CLI commands that don't run the service
    match cli.command {
        None | Some(Commands::Run) => {
            service_main(&cli.config_path).await;
        }
        Some(Commands::Config { config_command }) => {
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
                    token_env_var_name,
                    auto_update,
                    github_api_url,
                    auto_update_check_interval_sec,
                    package_format,
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
                        &token_env_var_name,
                        auto_update,
                        github_api_url,
                        auto_update_check_interval_sec,
                        package_format,
                    );
                }
                ConfigCommands::Validate => {
                    cli::config_validate(&cli.config_path);
                }
            }
            std::process::exit(0);
        }
        Some(Commands::RunOnce) => {
            if let Err(details) = run::run_once(&cli.config_path).await {
                report_error(&format!("On-demand pass failed: {}", details));
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        Some(Commands::Completion(completion_command)) => {
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
    // Self-update runs beside the compliance loop rather than inside it, so
    // a host whose startup keeps failing (repo unset, unreachable, or a
    // release that breaks run()) can still pick up a fixed release.
    tokio::spawn(update_loop(config_path.to_string()));

    loop {
        if let Err(details) = run(config_path).await {
            report_error(&format!(
                "unrecoverable error, retrying shortly: {}",
                details
            ));
        }
        sleep(Duration::from_secs(10)).await;
    }
}

/// Delay before re-reading the config when it fails to load. run() reports
/// the load error itself, so the update loop just waits for it to be fixed.
const UPDATE_CONFIG_RETRY: Duration = Duration::from_secs(10);

async fn update_loop(config_path: String) {
    let config = loop {
        match load_config(&config_path) {
            Ok(config) => break config,
            Err(_) => sleep(UPDATE_CONFIG_RETRY).await,
        }
    };

    // run() normally installs the subscriber, but it may not have yet.
    if !tracing::dispatcher::has_been_set() {
        init_tracing(&config.system_integration);
    }
    let auto_update_config = match (config.behavior.auto_update, config.auto_update) {
        (true, Some(auto_update_config)) => auto_update_config,
        // Validation guarantees the section is present when enabled.
        _ => return,
    };

    // Created only now: a span made before the subscriber exists stays
    // disabled. Instrumented rather than entered, as this task awaits.
    let global_span = span!(tracing::Level::INFO, "RegOps", hostname = ?get_hostname());
    update_checks(auto_update_config)
        .instrument(global_span)
        .await;
}

async fn update_checks(auto_update_config: AutoUpdateConfig) {
    info!(
        check_interval_sec = auto_update_config.check_interval_sec,
        github_api_url = %auto_update_config.github_api_url,
        "Auto-update enabled"
    );

    loop {
        match check_and_apply_update(&auto_update_config).await {
            Ok(UpdateOutcome::Updated) => {
                info!("Self-update installed; exiting so systemd restarts on the new binary");
                std::process::exit(0);
            }
            Ok(UpdateOutcome::UpToDate) => {}
            Err(details) => {
                warn!(
                    details,
                    "Self-update check failed; will retry next interval"
                );
            }
        }
        sleep(Duration::from_secs(auto_update_config.check_interval_sec)).await;
    }
}

fn get_hostname() -> String {
    match hostname::get() {
        Ok(value) => value.to_string_lossy().to_string(),
        Err(details) => {
            warn!(%details, "Failed to get hostname");
            "localhost".to_string()
        }
    }
}

async fn run(config_path: &str) -> Result<(), String> {
    let config = match load_config(config_path) {
        Ok(config) => config,
        Err(details) => return Err(format!("Failed to load configuration: {}", details)),
    };

    init_tracing(&config.system_integration);

    // Get hostname first for the global tracing span and for the managed host id
    let hostname = get_hostname();
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

    let host_id = config.host_id(&hostname);
    let reporter = match config.regency_reporting() {
        Some(regency) => {
            info!(endpoint = %regency.endpoint, host_id, "Reporting outcomes to regency");
            Some(Reporter::new(regency, &host_id)?)
        }
        None => None,
    };
    // Stopped when run() returns (e.g. to reload the configuration)
    let _sender = reporter.clone().map(|reporter| AbortOnDrop(tokio::spawn(reporter.run_sender())));

    // Operational loop
    loop {
        if let Err(details) =
            perform_compliance_pass(&config, &auth, &repo, &host_id, reporter.as_deref()).await
        {
            error!(
                details,
                "Failed to perform compliance pass, will retry next cycle"
            );
        }
        sleep(Duration::from_secs(config.behavior.interval_sec)).await;
    }
}

/// Aborts the task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
