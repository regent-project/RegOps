use gix::{Repository, Url};
use std::path::Path;
use tracing::info;

pub fn git_clone(
    repository_url: Url,
    local_path: &str,
    branch: &str,
) -> Result<Repository, String> {
    // Create local path if necessary
    std::fs::create_dir_all(local_path).map_err(|details| format!("{}", details))?;

    let local_path = Path::new(local_path);

    let ref_name = format!("refs/heads/{}", branch);

    let mut prepare_clone = gix::prepare_clone(repository_url, local_path)
        .map_err(|details| format!("{}", details))?
        .with_ref_name(Some(ref_name.as_str()))
        .map_err(|details| format!("{}", details))?;

    let (mut prepare_checkout, _) = prepare_clone
        .fetch_then_checkout(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
        .map_err(|details| format!("{}", details))?;

    let (repo, _) = prepare_checkout
        .main_worktree(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
        .map_err(|details| format!("{}", details))?;

    let head: String = match repo.head_commit() {
        Ok(commit) => commit.id().to_string(),
        Err(_details) => "unknown".to_string(),
    };

    info!(head, "Initial cloning done");

    Ok(repo)
}

pub fn git_pull(local_path: &str) -> Result<(), String> {
    // Open the existing repository
    let repo = gix::open(local_path).map_err(|details| format!("{}", details))?;

    // Get current HEAD and find the active local branch name
    let head = repo.head().map_err(|details| format!("{}", details))?;
    let local_branch_ref = head
        .referent_name()
        .ok_or("Repository is in a detached HEAD state")?;

    let branch_short_name = local_branch_ref
        .as_bstr()
        .strip_prefix(b"refs/heads/")
        .ok_or("HEAD does not point to a local branch under refs/heads/")?;
    let branch_name_str =
        std::str::from_utf8(branch_short_name).map_err(|details| format!("{}", details))?;

    // Determine the remote and tracking branch using Git config mapping
    // Falls back to "origin" and the current branch name if no upstream is explicitly set
    let (remote_name, remote_branch_name) = repo
        .branch_remote_ref_name(local_branch_ref, gix::remote::Direction::Fetch)
        .and_then(|remote_ref| {
            // Parse remote name and branch from the tracking reference if available
            // e.g., refs/remotes/origin/main -> ("origin", "main")
            let name_str = remote_ref.unwrap().to_string();
            if let Some(stripped) = name_str.strip_prefix("refs/remotes/") {
                let mut parts = stripped.splitn(2, '/');
                let r = parts.next()?;
                let b = parts.next()?;
                Some((r.to_string(), b.to_string()))
            } else {
                None
            }
        })
        .unwrap_or_else(|| ("origin".to_string(), branch_name_str.to_string()));

    // Connect and fetch from the detected remote
    let remote = repo
        .find_remote(&remote_name)
        .map_err(|details| format!("{}", details))?;

    remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|details| format!("{}", details))?
        .prepare_fetch(gix::progress::Discard, Default::default())
        .map_err(|details| format!("{}", details))?
        .receive(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
        .map_err(|details| format!("{}", details))?;

    // Resolve the remote-tracking reference
    let remote_ref_name = format!("refs/remotes/{}/{}", remote_name, remote_branch_name);
    let mut remote_ref = repo
        .find_reference(&remote_ref_name)
        .map_err(|details| format!("{}", details))?;
    let target_commit_id = remote_ref
        .peel_to_id()
        .map_err(|details| format!("{}", details))?;

    // Update local branch reference (Fast-forward)
    let mut local_ref = repo
        .find_reference(local_branch_ref.as_bstr())
        .map_err(|details| format!("{}", details))?;
    local_ref
        .set_target_id(target_commit_id.detach(), "gix auto-pull: Fast-forward")
        .map_err(|details| format!("{}", details))?;

    // Update index and check out files into the working directory
    let commit = target_commit_id
        .object()
        .map_err(|details| format!("{}", details))?
        .peel_to_commit()
        .map_err(|details| format!("{}", details))?;
    let tree_id = commit.tree_id().map_err(|details| format!("{}", details))?;

    let mut index = repo
        .index_from_tree(&tree_id)
        .map_err(|details| format!("{}", details))?;
    index
        .write(gix::index::write::Options::default())
        .map_err(|details| format!("{}", details))?;

    if let Some(workdir) = repo.workdir() {
        gix::worktree::state::checkout(
            &mut index,
            workdir,
            repo.objects.clone(),
            &gix::progress::Discard,
            &gix::progress::Discard,
            &Default::default(),
            Default::default(),
        )
        .map_err(|details| format!("{}", details))?;
    }

    let head: String = match repo.head_commit() {
        Ok(commit) => commit.id().to_string(),
        Err(_details) => "unknown".to_string(),
    };

    info!(head, "Git pull done");

    Ok(())
}
