use regent_sdk::ExpectedState;
use regent_sdk::hosts::handlers::{ConnectionMethod, TargetUser};
use regent_sdk::hosts::managed_host::{ManagedHost, ManagedHostBuilder};
use tracing::{error, info, span, warn};

use crate::common::{GitAuthentication, RunningMode, init_tracing};
use crate::config::{RegOpsConfig, load_config};
use crate::git::{clone_fresh, git_pull, local_repo_matches_expected};

pub async fn run_once(config_path: &str) -> Result<(), String> {
    let config = match load_config(config_path) {
        Ok(config) => config,
        Err(details) => return Err(format!("Failed to load configuration: {}", details)),
    };

    init_tracing(&config.system_integration);

    let hostname = match hostname::get() {
        Ok(value) => value.to_string_lossy().to_string(),
        Err(details) => {
            warn!(%details, "Failed to get hostname");
            "localhost".to_string()
        }
    };
    let global_span = span!(tracing::Level::INFO, "RegOps Once", ?hostname);
    let _guard = global_span.enter();

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

    if !local_repo_matches_expected(&config.git.local_path, &repo, &config.git.branch) {
        match clone_fresh(&config.git.local_path, &repo, &config.git.branch, &auth) {
            Ok(()) => {}
            Err(details) => return Err(format!("Failed initial cloning: {}", details)),
        }
    }

    match git_pull(&config.git.local_path, &auth) {
        Ok(()) => {}
        Err(details) => {
            warn!(
                details,
                "Failed to pull git repository, wiping local copy and re-cloning"
            );
            match clone_fresh(&config.git.local_path, &repo, &config.git.branch, &auth) {
                Ok(()) => {}
                Err(recovery_details) => {
                    error!(recovery_details, "Failed to recover local git repository");
                    return Err(recovery_details);
                }
            }
        }
    }

    let expected_state_description = match std::fs::read_to_string(format!(
        "{}/{}",
        config.git.local_path, config.git.expected_state_path
    )) {
        Ok(content) => content,
        Err(details) => {
            error!(?details, "Failed to get file content");
            return Err(format!("Failed to read expected state file: {}", details));
        }
    };

    let expected_state = match ExpectedState::from_raw_yaml(&expected_state_description) {
        Ok(state) => state,
        Err(error_detail) => {
            error!("Wrong yaml content : {:?}", error_detail);
            return Err(format!("Invalid expected state YAML: {:?}", error_detail));
        }
    };

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

    match &config.behavior.mode {
        RunningMode::Assess => {
            match managed_localhost
                .assess_compliance(&expected_state, true)
                .await
            {
                Ok(_assessment) => {}
                Err(details) => warn!(%details, "Failed to assess compliance"),
            }
        }
        RunningMode::Enforce => match managed_localhost.reach_compliance(&expected_state).await {
            Ok(_outcome) => {}
            Err(details) => warn!(%details, "Failed to enforce compliance"),
        },
    }

    info!("On-demand automation pass completed");
    Ok(())
}

pub async fn perform_compliance_pass(
    managed_localhost: &mut ManagedHost,
    config: &RegOpsConfig,
    auth: &GitAuthentication,
    repo: &str,
) -> Result<(), String> {
    if !local_repo_matches_expected(&config.git.local_path, repo, &config.git.branch) {
        match clone_fresh(&config.git.local_path, repo, &config.git.branch, auth) {
            Ok(()) => {}
            Err(details) => return Err(format!("Failed initial cloning: {}", details)),
        }
    }

    match git_pull(&config.git.local_path, auth) {
        Ok(()) => {}
        Err(details) => {
            warn!(
                details,
                "Failed to pull git repository, wiping local copy and re-cloning"
            );
            match clone_fresh(&config.git.local_path, repo, &config.git.branch, auth) {
                Ok(()) => {}
                Err(recovery_details) => {
                    error!(
                        recovery_details,
                        "Failed to recover local git repository, will retry next cycle"
                    );
                    return Err(recovery_details);
                }
            }
        }
    }

    let expected_state_description = match std::fs::read_to_string(format!(
        "{}/{}",
        config.git.local_path, config.git.expected_state_path
    )) {
        Ok(content) => content,
        Err(details) => {
            error!(?details, "Failed to get file content");
            return Err(format!("Failed to read expected state file: {}", details));
        }
    };

    let expected_state = match ExpectedState::from_raw_yaml(&expected_state_description) {
        Ok(state) => state,
        Err(error_detail) => {
            error!("Wrong yaml content : {:?}", error_detail);
            return Err(format!("Invalid expected state YAML: {:?}", error_detail));
        }
    };

    match &config.behavior.mode {
        RunningMode::Assess => {
            match managed_localhost
                .assess_compliance(&expected_state, true)
                .await
            {
                Ok(_assessment) => {}
                Err(details) => warn!(%details, "Failed to assess compliance"),
            }
        }
        RunningMode::Enforce => match managed_localhost.reach_compliance(&expected_state).await {
            Ok(_outcome) => {}
            Err(details) => warn!(%details, "Failed to enforce compliance"),
        },
    }

    Ok(())
}
