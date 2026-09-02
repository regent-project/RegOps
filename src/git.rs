use gix::{Repository, Url};
use std::fs;
use std::io::Write;
use std::path::Path;
use tracing::{info, warn};

use crate::config::GitAuthentication;

fn token_credentials(
    token: &str,
) -> impl FnMut(gix::credentials::helper::Action) -> gix::credentials::protocol::Result + 'static {
    let token = token.to_owned();

    move |action| match action {
        gix::credentials::helper::Action::Get(context) => {
            Ok(Some(gix::credentials::protocol::Outcome {
                identity: gix::sec::identity::Account {
                    username: "oauth2".to_string(),
                    password: token.clone(),
                    oauth_refresh_token: None,
                },
                next: context.into(),
            }))
        }
        gix::credentials::helper::Action::Store(_) | gix::credentials::helper::Action::Erase(_) => {
            Ok(None)
        }
    }
}

pub fn wipe_local_repo(local_path: &str) -> Result<(), String> {
    match fs::remove_dir_all(local_path) {
        Ok(()) => {}
        Err(details) => match details.kind() {
            std::io::ErrorKind::NotFound => {}
            _ => return Err(format!("Failed to remove local path '{}': {}", local_path, details)),
        },
    }

    match fs::create_dir_all(local_path) {
        Ok(()) => Ok(()),
        Err(details) => Err(format!("Failed to recreate local path '{}': {}", local_path, details)),
    }
}

pub fn clone_fresh(local_path: &str, repo: &str, branch: &str, auth: &GitAuthentication) -> Result<(), String> {
    match wipe_local_repo(local_path) {
        Ok(()) => {}
        Err(details) => return Err(details),
    }

    let repository_url = match gix::url::parse(repo) {
        Ok(url) => url,
        Err(details) => return Err(format!("Failed to parse repository URL '{}': {}", repo, details)),
    };

    match git_clone(repository_url, local_path, branch, auth) {
        Ok(_repo) => Ok(()),
        Err(details) => Err(details),
    }
}

pub fn local_repo_matches_expected(local_path: &str, expected_remote_url: &str, expected_branch: &str) -> bool {
    let repo = match gix::open(local_path) {
        Ok(repo) => repo,
        Err(_details) => return false,
    };

    let remote = match repo.find_remote("origin") {
        Ok(remote) => remote,
        Err(_details) => return false,
    };

    let current_remote_url = match remote.url(gix::remote::Direction::Fetch) {
        Some(url) => url,
        None => return false,
    };

    if current_remote_url.to_bstring() != expected_remote_url {
        return false;
    }

    let head = match repo.head() {
        Ok(head) => head,
        Err(_details) => return false,
    };

    let local_branch_ref = match head.referent_name() {
        Some(name) => name,
        None => return false, // Detached HEAD: not the branch we expect to be on.
    };

    let expected_ref_name = format!("refs/heads/{}", expected_branch);

    local_branch_ref.as_bstr() == expected_ref_name.as_str()
}

pub fn git_clone(
    repository_url: Url,
    local_path: &str,
    branch: &str,
    auth: &GitAuthentication,
) -> Result<Repository, String> {
    match fs::create_dir_all(local_path) {
        Ok(()) => {}
        Err(details) => return Err(format!("Failed to create local path '{}': {}", local_path, details)),
    }

    let local_path_dir = Path::new(local_path);
    let ref_name = format!("refs/heads/{}", branch);

    let prepare_clone = match gix::prepare_clone(repository_url, local_path_dir) {
        Ok(prepare_clone) => prepare_clone,
        Err(details) => return Err(format!("Failed to prepare clone: {}", details)),
    };

    let mut prepare_clone = match prepare_clone.with_ref_name(Some(ref_name.as_str())) {
        Ok(prepare_clone) => prepare_clone,
        Err(details) => return Err(format!("Failed to set ref name '{}': {}", ref_name, details)),
    };

    if let GitAuthentication::WithToken(token) = auth {
        let token = token.clone();
        prepare_clone = prepare_clone.configure_connection(move |connection| {
            connection.set_credentials(token_credentials(&token));
            Ok(())
        });
    }

    let (mut prepare_checkout, _) =
        match prepare_clone.fetch_then_checkout(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED) {
            Ok(result) => result,
            Err(details) => return Err(format!("Failed to fetch during clone: {}", details)),
        };

    let (repo, _) = match prepare_checkout.main_worktree(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED) {
        Ok(result) => result,
        Err(details) => return Err(format!("Failed to checkout main worktree during clone: {}", details)),
    };

    // Locally unset core.attributesfile to prevent gix from failing on missing global files.
    // Best-effort: a failure here doesn't invalidate the clone itself.
    let config_path = repo.git_dir().join("config");
    match fs::OpenOptions::new().append(true).open(&config_path) {
        Ok(mut file) => {
            if let Err(details) = writeln!(file, "\n[core]\n    attributesFile = \"\"") {
                warn!(%details, "Failed to patch git config after clone");
            }
        }
        Err(details) => warn!(%details, "Failed to open git config after clone"),
    }

    let head: String = match repo.head_commit() {
        Ok(commit) => commit.id().to_string(),
        Err(_details) => "unknown".to_string(),
    };

    info!(head, "Initial cloning done");

    Ok(repo)
}

pub fn git_pull(local_path: &str, auth: &GitAuthentication) -> Result<(), String> {
    // Inject local committer config before opening the repo so gix loads it into memory.
    // Best-effort: a failure here doesn't invalidate the pull itself.
    let config_path = Path::new(local_path).join(".git").join("config");
    match fs::OpenOptions::new().create(true).append(true).open(&config_path) {
        Ok(mut file) => {
            if let Err(details) = writeln!(file, "\n[user]\n    name = Local User\n    email = user@local.invalid") {
                warn!(%details, "Failed to patch git config before pull");
            }
        }
        Err(details) => warn!(%details, "Failed to open git config before pull"),
    }

    // Open the existing repository
    let repo = match gix::open(local_path) {
        Ok(repo) => repo,
        Err(details) => return Err(format!("Failed to open repository at '{}': {}", local_path, details)),
    };

    // Get current HEAD and find the active local branch name
    let head = match repo.head() {
        Ok(head) => head,
        Err(details) => return Err(format!("Failed to get HEAD: {}", details)),
    };

    let local_branch_ref = match head.referent_name() {
        Some(name) => name,
        None => return Err("Repository is in a detached HEAD state".to_string()),
    };

    let branch_short_name = match local_branch_ref.as_bstr().strip_prefix(b"refs/heads/") {
        Some(short_name) => short_name,
        None => return Err("HEAD does not point to a local branch under refs/heads/".to_string()),
    };

    let branch_name_str = match std::str::from_utf8(branch_short_name) {
        Ok(name) => name,
        Err(details) => return Err(format!("Branch name is not valid UTF-8: {}", details)),
    };

    // Determine the remote and tracking branch using Git config mapping
    // Falls back to "origin" and the current branch name if no upstream is explicitly set
    let (remote_name, remote_branch_name) = match repo.branch_remote_ref_name(local_branch_ref, gix::remote::Direction::Fetch) {
        Some(Ok(remote_ref)) => {
            // Parse remote name and branch from the tracking reference if available
            // e.g., refs/remotes/origin/main -> ("origin", "main")
            let name_str = remote_ref.to_string();
            match name_str.strip_prefix("refs/remotes/") {
                Some(stripped) => {
                    let mut parts = stripped.splitn(2, '/');
                    match (parts.next(), parts.next()) {
                        (Some(remote), Some(branch)) => (remote.to_string(), branch.to_string()),
                        _ => ("origin".to_string(), branch_name_str.to_string()),
                    }
                }
                None => ("origin".to_string(), branch_name_str.to_string()),
            }
        }
        Some(Err(_details)) => ("origin".to_string(), branch_name_str.to_string()),
        None => ("origin".to_string(), branch_name_str.to_string()),
    };

    // Connect and fetch from the detected remote
    let remote = match repo.find_remote(&remote_name) {
        Ok(remote) => remote,
        Err(details) => return Err(format!("Failed to find remote '{}': {}", remote_name, details)),
    };

    let mut connected_remote = match remote.connect(gix::remote::Direction::Fetch) {
        Ok(connected_remote) => connected_remote,
        Err(details) => return Err(format!("Failed to connect to remote '{}': {}", remote_name, details)),
    };

    // Must be set before `prepare_fetch`, which is what performs the handshake.
    if let GitAuthentication::WithToken(token) = auth {
        connected_remote.set_credentials(token_credentials(token));
    }

    let prepared_fetch = match connected_remote.prepare_fetch(gix::progress::Discard, Default::default()) {
        Ok(prepared_fetch) => prepared_fetch,
        Err(details) => return Err(format!("Failed to prepare fetch: {}", details)),
    };

    match prepared_fetch.receive(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED) {
        Ok(_outcome) => {}
        Err(details) => return Err(format!("Failed to fetch from remote '{}': {}", remote_name, details)),
    };

    // Resolve the remote-tracking reference
    let remote_ref_name = format!("refs/remotes/{}/{}", remote_name, remote_branch_name);
    let mut remote_ref = match repo.find_reference(&remote_ref_name) {
        Ok(remote_ref) => remote_ref,
        Err(details) => return Err(format!("Failed to find reference '{}': {}", remote_ref_name, details)),
    };

    let target_commit_id = match remote_ref.peel_to_id() {
        Ok(target_commit_id) => target_commit_id,
        Err(details) => return Err(format!("Failed to peel reference '{}' to a commit: {}", remote_ref_name, details)),
    };

    // Update local branch reference (Fast-forward)
    let mut local_ref = match repo.find_reference(local_branch_ref.as_bstr()) {
        Ok(local_ref) => local_ref,
        Err(details) => return Err(format!("Failed to find local branch reference: {}", details)),
    };

    match local_ref.set_target_id(target_commit_id.detach(), "gix auto-pull: Fast-forward") {
        Ok(_) => {}
        Err(details) => return Err(format!("Failed to fast-forward local branch: {}", details)),
    };

    // Update index and check out files into the working directory
    let commit_object = match target_commit_id.object() {
        Ok(commit_object) => commit_object,
        Err(details) => return Err(format!("Failed to resolve commit object: {}", details)),
    };

    let commit = match commit_object.peel_to_commit() {
        Ok(commit) => commit,
        Err(details) => return Err(format!("Failed to peel object to commit: {}", details)),
    };

    let tree_id = match commit.tree_id() {
        Ok(tree_id) => tree_id,
        Err(details) => return Err(format!("Failed to get tree id from commit: {}", details)),
    };

    let mut index = match repo.index_from_tree(&tree_id) {
        Ok(index) => index,
        Err(details) => return Err(format!("Failed to build index from tree: {}", details)),
    };

    match index.write(gix::index::write::Options::default()) {
        Ok(_) => {}
        Err(details) => return Err(format!("Failed to write index: {}", details)),
    };

    if let Some(workdir) = repo.workdir() {
        match gix::worktree::state::checkout(
            &mut index,
            workdir,
            repo.objects.clone(),
            &gix::progress::Discard,
            &gix::progress::Discard,
            &Default::default(),
            Default::default(),
        ) {
            Ok(_) => {}
            Err(details) => return Err(format!("Failed to checkout working directory: {}", details)),
        };
    }

    let head: String = match repo.head_commit() {
        Ok(commit) => commit.id().to_string(),
        Err(_details) => "unknown".to_string(),
    };

    info!(head, "Git pull done");

    Ok(())
}
