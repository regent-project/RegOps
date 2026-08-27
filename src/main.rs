use std::process::exit;
use std::{fs::File, io::Read};
use tokio::time::{Duration, sleep};
use tracing::{error, warn, info};
use regent_sdk::hosts::managed_host::ManagedHostBuilder;
use regent_sdk::hosts::handlers::{ConnectionMethod, TargetUser};
use regent_sdk::ExpectedState;

mod config;
mod git;

use crate::config::{RegOpsConfig, RunningMode};
use crate::git::{git_clone, git_pull};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Getting configuration
    let mut configuration_file = File::open("examples/config.toml").unwrap();
    let mut file_content: Vec<u8> = Vec::new();
    configuration_file.read_to_end(&mut file_content).unwrap();

    let config: RegOpsConfig = toml::from_slice(&file_content).unwrap();

    std::fs::create_dir_all(&config.git.local_path).unwrap();
    let repository_url = gix::url::parse(&config.git.repo).unwrap();

    info!("Git repository : {:?}", repository_url);
    info!("Running mode : {:?}", config.behavior.mode);

    // Check if repository is already present
    // Initial cloning of the repository

    let mut initial_cloning_required = false;

    match gix::open(&config.git.local_path) {
        Ok(already_existing_repository) => {
            // There is already a repository. Check that it is the expected one.
            match already_existing_repository.find_remote("origin") {
                Ok(remote) => {
                    match remote.url(gix::remote::Direction::Fetch) {
                        Some(current_remote_url) => {
                            if current_remote_url.to_bstring() == config.git.repo.as_str() {
                                // The current repository is the expected one. No initial cloning but a "git pull" required to get up to date.
                                git_pull(&config.git.local_path).unwrap();
                            } else {
                                // The current repository is not the expected one.
                                // There is ambiguity. Abort.
                                error!(
                                    "There is already a git repository in place, but not the expected one."
                                );
                                exit(1);
                            }
                        }
                        None => {
                            initial_cloning_required = true;
                        }
                    }
                }
                Err(_details) => {
                    initial_cloning_required = true;
                }
            }
        }
        Err(_details) => {
            initial_cloning_required = true;
        }
    }

    if initial_cloning_required {
        // There is no repository in here, initial cloning required
        git_clone(repository_url, &config.git.local_path, &config.git.branch).unwrap();
    }

    // Regent initialization
    let mut managed_localhost = ManagedHostBuilder::new(
            "localhost", 
            "localhost", 
            Some(ConnectionMethod::Localhost(TargetUser::current_user()))
        )
        .build(None)
        .await
        .unwrap();
    managed_localhost.connect().await.unwrap();


    // Entering the infinite loop
    loop {
        // Git part to get up to date with expected state
        git_pull(&config.git.local_path).unwrap();

        // Regent part
        let expected_state_description = match std::fs::read_to_string(&config.git.expected_state_path) {
            Ok(content) => content,
            Err(details) => {
                error!(?details, "Failed to get file content");
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
                if let Err(details) = managed_localhost
                    .assess_compliance(&expected_state, true)
                    .await {
                        warn!(?details, "Failed to assess compliance");
                    }
            }
            RunningMode::Enforce => {
                if let Err(details) = managed_localhost
                    .reach_compliance(&expected_state)
                    .await {
                        warn!(?details, "Failed to enforce compliance");
                    }
            }
        }

        sleep(Duration::from_secs(config.behavior.interval_sec)).await;
    }
}
