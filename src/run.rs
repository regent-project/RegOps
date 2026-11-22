use regent_sdk::hosts::handlers::{ConnectionMethod, TargetUser};
use regent_sdk::hosts::live::RunLog;
use regent_sdk::hosts::managed_host::ManagedHostBuilder;
use regent_sdk::state::compliance::HostStatus;
use regent_sdk::task::{Job, RegentTask, RegentTaskResult};
use regent_sdk::{ExpectedState, RegentError};
use tracing::{error, info, span, warn};

use crate::common::{GitAuthentication, RunningMode, init_tracing};
use crate::config::{RegOpsConfig, load_config};
use crate::git::{clone_fresh, git_pull, head_commit, local_repo_matches_expected};
use crate::reporting::{Reporter, StateSource};

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

    let host_id = config.host_id(&hostname);
    let reporter = match config.regency_reporting() {
        Some(regency) => Some(Reporter::new(regency, &host_id)?),
        None => None,
    };

    let source = state_source(&config, &expected_state);
    let (result, run_log) = run_task(&config, &host_id, expected_state, reporter.as_deref()).await;
    log_outcome(&result);
    if let Some(reporter) = &reporter {
        reporter.record(&result, &source, run_log.as_ref())?;
        // Best effort: what cannot be sent now stays spooled for the service
        if let Err(details) = reporter.flush().await {
            warn!(details, "Outcome spooled but not reported yet");
        }
    }

    info!("On-demand automation pass completed");
    Ok(())
}

/// One pass: sync the repository, run the expected state as a `RegentTask` on
/// localhost (so the outcome is the same `RegentTaskResult` remote runners
/// produce), log it and hand it to the reporter if any.
pub async fn perform_compliance_pass(
    config: &RegOpsConfig,
    auth: &GitAuthentication,
    repo: &str,
    host_id: &str,
    reporter: Option<&Reporter>,
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

    let source = state_source(config, &expected_state);
    let (result, run_log) = run_task(config, host_id, expected_state, reporter).await;
    log_outcome(&result);
    if let Some(reporter) = reporter {
        reporter.record(&result, &source, run_log.as_ref())?;
    }

    Ok(())
}

/// Run the expected state on localhost as a `RegentTask`. Always returns a
/// result: failures (host busy, connection, …) are inside it. With a reporter,
/// the pass runs live (streamed to regency) and its log is returned too.
async fn run_task(
    config: &RegOpsConfig,
    host_id: &str,
    expected_state: ExpectedState,
    reporter: Option<&Reporter>,
) -> (RegentTaskResult, Option<RunLog>) {
    // We expect the user which runs Regops to have required permissions with
    // non-interactive sudo capability.
    let mut managed_host_builder = ManagedHostBuilder::new(
        host_id,
        "localhost",
        Some(ConnectionMethod::Localhost(TargetUser::current_user())),
    );
    managed_host_builder.set_host_lock(config.host_lock_settings());
    let job = match config.behavior.mode {
        RunningMode::Assess => Job::Assess,
        RunningMode::Enforce => Job::Reach,
    };
    let mut task = RegentTask::from(managed_host_builder, expected_state, job);
    let Some(reporter) = reporter else {
        return (task.run(None).await, None);
    };
    let (sink, live) = reporter.start_live();
    task.enable_live(Some(sink));
    let result = task.run(None).await;
    task.disable_live(); // drops the sink: the collector ends
    (result, Some(live.finish().await))
}

fn log_outcome(result: &RegentTaskResult) {
    match result.outcome() {
        Ok(status) => match status.state {
            HostStatus::AlreadyCompliant => info!("Already compliant"),
            HostStatus::NotCompliant => {
                warn!(remediations = ?status.all_remediations(), "Not compliant")
            }
            HostStatus::ReachComplianceSuccess => info!("Compliance reached"),
            HostStatus::ReachComplianceFailed => warn!("Failed to reach compliance"),
            HostStatus::Aborted => warn!("Pass aborted"),
        },
        Err(RegentError::HostBusy(details)) => {
            info!(
                details,
                "Host busy with another Regent operation, skipping this pass"
            )
        }
        Err(details) => warn!(%details, "Compliance pass failed"),
    }
}

/// What regency is told about the expected state of a pass (D28): the
/// repository, branch, file and commit it comes from, and its content.
fn state_source(config: &RegOpsConfig, expected_state: &ExpectedState) -> StateSource {
    StateSource {
        revision: head_commit(&config.git.local_path),
        repo: config.git.repo.clone().map(|r| redact_url(&r)).unwrap_or_default(),
        branch: config.git.branch.clone(),
        path: config.git.expected_state_path.clone(),
        expected_state_json: serde_json::to_string(expected_state).ok(),
    }
}

/// A repository URL without credentials (`https://user:token@host/…`).
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find('/').unwrap_or(rest.len());
    match rest[..end].rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://{host}{}", &rest[end..]),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod source_tests {
    use super::redact_url;

    #[test]
    fn credentials_never_leave_the_host() {
        assert_eq!(
            redact_url("https://bot:secret@git.example/infra.git"),
            "https://git.example/infra.git"
        );
        assert_eq!(redact_url("https://git.example/infra.git"), "https://git.example/infra.git");
        assert_eq!(
            redact_url("https://git.example/a@b/infra.git"),
            "https://git.example/a@b/infra.git"
        );
    }
}
