use std::{
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    io,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};

use crate::{
    model::{Config, DEFAULT_BASE_BRANCH, Project},
    paths::same_path,
};

pub(crate) struct DetachedWorktreePlan {
    pub(crate) temporary_branch: String,
    pub(crate) base_commit: String,
}

pub(crate) fn detached_worktree_plan(
    repository: &Path,
    base: &str,
) -> Result<DetachedWorktreePlan> {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("read system clock")?
        .as_nanos();
    detached_worktree_plan_at(repository, base, seed)
}

fn detached_worktree_plan_at(
    repository: &Path,
    base: &str,
    seed: u128,
) -> Result<DetachedWorktreePlan> {
    let base_ref = format!("{base}^{{commit}}");
    let base_commit = git_stdout(repository, ["rev-parse", &base_ref])?;
    for attempt in 0..16 {
        let temporary_branch = short_worktree_name(seed, attempt);
        let reference = format!("refs/heads/{temporary_branch}");
        let status = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(["show-ref", "--verify", "--quiet", &reference])
            .status()
            .with_context(|| format!("inspect Git branches at {}", repository.display()))?;
        match status.code() {
            Some(1) => {
                return Ok(DetachedWorktreePlan {
                    temporary_branch,
                    base_commit,
                });
            }
            Some(0) => continue,
            _ => bail!("Could not inspect temporary Git branch {temporary_branch:?}"),
        }
    }
    bail!("Could not allocate a temporary Git branch")
}

fn short_worktree_name(seed: u128, attempt: u64) -> String {
    let mut hasher = DefaultHasher::new();
    (seed, std::process::id(), attempt).hash(&mut hasher);
    format!("{:010x}", hasher.finish() & 0xff_ffff_ffff)
}

pub(crate) fn detach_created_worktree(
    repository: &Path,
    worktree: &Path,
    temporary_branch: &str,
) -> Result<()> {
    run_git(worktree, ["checkout", "--detach"])
        .with_context(|| format!("detach worktree at {}", worktree.display()))?;
    let reference = format!("refs/heads/{temporary_branch}");
    let reference_status = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["show-ref", "--verify", "--quiet", &reference])
        .status()
        .with_context(|| format!("inspect temporary Git branch {temporary_branch:?}"))?;
    match reference_status.code() {
        Some(1) => return Ok(()),
        Some(0) => {}
        _ => bail!("Could not inspect temporary Git branch {temporary_branch:?}"),
    }
    let temporary_oid = git_stdout(repository, ["rev-parse", &reference])?;
    run_git(
        repository,
        [
            "update-ref",
            "-d",
            reference.as_str(),
            temporary_oid.as_str(),
        ],
    )
    .with_context(|| format!("remove temporary Git branch {temporary_branch:?}"))
}

pub(crate) fn worktree_path_for_branch(repository: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .with_context(|| format!("list Git worktrees at {}", repository.display()))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!("Git command failed: {message}");
    }
    let output = String::from_utf8(output.stdout).context("Git output is not UTF-8")?;
    let target = format!("refs/heads/{branch}");
    let mut path = None;
    for line in output.lines() {
        if line.is_empty() {
            path = None;
        } else if let Some(value) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(value));
        } else if line.strip_prefix("branch ") == Some(target.as_str()) {
            return Ok(path);
        }
    }
    Ok(None)
}

pub(crate) fn remove_worktree(repository: &Path, worktree: &Path) -> Result<()> {
    let (exists, registered_worktree) = removable_worktree(repository, worktree)?;
    let Some(registered_worktree) = registered_worktree else {
        return Ok(());
    };
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["worktree", "remove"])
        .arg(&registered_worktree)
        .output()
        .with_context(|| format!("remove Git worktree at {}", worktree.display()))?;
    if output.status.success() {
        if !exists {
            run_git(repository, ["worktree", "prune", "--expire", "now"])
                .context("prune missing Git worktree metadata")?;
        }
        return Ok(());
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    bail!("Git refused to remove the worktree: {message}")
}

pub(crate) fn validate_worktree_removal(repository: &Path, worktree: &Path) -> Result<()> {
    removable_worktree(repository, worktree).map(|_| ())
}

fn removable_worktree(repository: &Path, worktree: &Path) -> Result<(bool, Option<PathBuf>)> {
    let exists = worktree
        .try_exists()
        .with_context(|| format!("inspect worktree at {}", worktree.display()))?;
    let registered = registered_worktrees(repository)?;
    let registered_worktree = registered
        .into_iter()
        .find(|registered| equivalent_worktree_path(registered, worktree));
    let Some(registered_worktree) = registered_worktree else {
        if exists {
            bail!(
                "Path {} is not a registered Git worktree",
                worktree.display()
            );
        }
        return Ok((false, None));
    };
    if exists {
        ensure_worktree_safe_to_remove(worktree)?;
    }
    Ok((exists, Some(registered_worktree)))
}

fn equivalent_worktree_path(left: &Path, right: &Path) -> bool {
    if same_path(left, right) {
        return true;
    }
    let canonicalize_parent =
        |path: &Path| Some(path.parent()?.canonicalize().ok()?.join(path.file_name()?));
    canonicalize_parent(left) == canonicalize_parent(right)
}

pub(crate) fn ensure_worktree_safe_to_remove(worktree: &Path) -> Result<()> {
    if worktree
        .try_exists()
        .with_context(|| format!("inspect worktree at {}", worktree.display()))?
    {
        let status = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["status", "--porcelain"])
            .output()
            .with_context(|| format!("inspect Git status at {}", worktree.display()))?;
        if !status.status.success() {
            bail!(
                "Could not inspect Git status at {}: {}",
                worktree.display(),
                String::from_utf8_lossy(&status.stderr).trim()
            );
        }
        if !status.stdout.is_empty() {
            bail!(
                "Worktree {} has uncommitted changes; commit or discard them before deleting the session",
                worktree.display()
            );
        }
        ensure_head_is_referenced(worktree)?;
    }
    Ok(())
}

fn registered_worktrees(repository: &Path) -> Result<Vec<PathBuf>> {
    let output = git_stdout(repository, ["worktree", "list", "--porcelain"])?;
    Ok(output
        .lines()
        .filter_map(|line| line.strip_prefix("worktree ").map(PathBuf::from))
        .collect())
}

fn ensure_head_is_referenced(worktree: &Path) -> Result<()> {
    let symbolic_head = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
        .with_context(|| format!("inspect Git HEAD at {}", worktree.display()))?;
    if symbolic_head.status.success() {
        return Ok(());
    }
    if symbolic_head.status.code() != Some(1) {
        bail!(
            "Could not inspect Git HEAD at {}: {}",
            worktree.display(),
            String::from_utf8_lossy(&symbolic_head.stderr).trim()
        );
    }
    let head = git_stdout(worktree, ["rev-parse", "HEAD"])?;
    let refs = git_stdout(
        worktree,
        ["for-each-ref", "--format=%(refname)", "--contains", &head],
    )?;
    if refs.is_empty() {
        bail!(
            "Worktree HEAD {head} is not reachable from a branch or tag; create one before deleting the session"
        );
    }
    Ok(())
}

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .with_context(|| format!("run Git at {}", cwd.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    bail!("Git command failed: {message}")
}

fn git_stdout<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .with_context(|| format!("run Git at {}", cwd.display()))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!("Git command failed: {message}");
    }
    String::from_utf8(output.stdout)
        .context("Git output is not UTF-8")
        .map(|value| value.trim().to_owned())
}

pub(crate) fn normalize_project(mut project: Project, config: &Config) -> Result<Project> {
    let path = fs::canonicalize(&project.path)
        .with_context(|| format!("open project path {}", project.path.display()))?;
    if config
        .projects
        .iter()
        .any(|existing| same_path(&existing.path, &path))
    {
        bail!("Project path {} is already configured", path.display());
    }
    let output = Command::new("git")
        .args(["-C"])
        .arg(&path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .with_context(|| format!("inspect Git repository at {}", path.display()))?;
    let root = if output.status.success() {
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
    } else if has_git_metadata(&path)? {
        bail!(
            "Could not inspect existing Git metadata at {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    } else {
        initialize_repository(&path)?;
        path.clone()
    };
    if !same_path(&path, &root) {
        bail!(
            "Project path must be the Git checkout root: {}",
            root.display()
        );
    }
    let bare = Command::new("git")
        .args(["-C"])
        .arg(&path)
        .args(["rev-parse", "--is-bare-repository"])
        .output()
        .with_context(|| format!("inspect Git repository type at {}", path.display()))?;
    if !bare.status.success() {
        bail!(
            "Could not inspect Git repository type at {}: {}",
            path.display(),
            String::from_utf8_lossy(&bare.stderr).trim()
        );
    }
    if String::from_utf8_lossy(&bare.stdout).trim() == "true" {
        bail!("Project path {} is a bare Git repository", path.display());
    }
    project.base_branch = detect_base_branch(&path)?;
    ensure_initial_commit(&path, &project.base_branch)?;
    project.path = path;
    Ok(project)
}

pub(crate) fn repair_base_branch(project: &mut Project) -> Result<bool> {
    if git_ref_resolves(&project.path, &project.base_branch)? {
        return Ok(false);
    }
    let detected = detect_base_branch(&project.path)?;
    if resolve_git_commit(&project.path, &detected)?.is_none() {
        bail!(
            "Detected base {detected:?} does not resolve for project {:?}",
            project.name
        );
    }
    if detected == project.base_branch {
        return Ok(false);
    }
    project.base_branch = detected;
    Ok(true)
}

fn initialize_repository(path: &Path) -> Result<()> {
    let initial_branch = preferred_initial_branch(path)?;
    let initialized = Command::new("git")
        .args(["-C"])
        .arg(path)
        .arg("init")
        .output()
        .with_context(|| format!("initialize Git repository at {}", path.display()))?;
    if !initialized.status.success() {
        bail!(
            "Could not initialize Git repository at {}: {}",
            path.display(),
            String::from_utf8_lossy(&initialized.stderr).trim()
        );
    }
    select_head_branch(path, &initial_branch)?;
    Ok(())
}

fn preferred_initial_branch(path: &Path) -> Result<String> {
    let configured = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["config", "--get", "init.defaultBranch"])
        .output()
        .context("read Git initial branch configuration")?;
    let branch = if configured.status.success() {
        String::from_utf8(configured.stdout)
            .context("Git initial branch is not UTF-8")?
            .trim()
            .to_owned()
    } else if configured.status.code() == Some(1) {
        DEFAULT_BASE_BRANCH.to_owned()
    } else {
        bail!(
            "Could not read Git initial branch configuration: {}",
            String::from_utf8_lossy(&configured.stderr).trim()
        );
    };
    validate_branch_name(&branch)?;
    Ok(branch)
}

fn detect_base_branch(path: &Path) -> Result<String> {
    if let Some(remote) = symbolic_ref(path, "refs/remotes/origin/HEAD")? {
        if let Some((_, local)) = remote.split_once('/')
            && git_ref_resolves(path, &format!("refs/heads/{local}"))?
        {
            return Ok(local.to_owned());
        }
        if git_ref_resolves(path, &remote)? {
            return Ok(remote);
        }
    }

    let current = symbolic_ref(path, "HEAD")?;
    if let Some(current @ ("main" | "master")) = current.as_deref()
        && git_ref_resolves(path, &format!("refs/heads/{current}"))?
    {
        return Ok(current.to_owned());
    }
    for candidate in ["main", "master"] {
        if git_ref_resolves(path, &format!("refs/heads/{candidate}"))? {
            return Ok(candidate.to_owned());
        }
    }
    if let Some(current) = current.as_ref()
        && git_ref_resolves(path, &format!("refs/heads/{current}"))?
    {
        return Ok(current.clone());
    }

    let branches = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args([
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname:short)",
            "refs/heads",
        ])
        .output()
        .with_context(|| format!("list local Git branches at {}", path.display()))?;
    if !branches.status.success() {
        bail!(
            "Could not list local Git branches at {}: {}",
            path.display(),
            String::from_utf8_lossy(&branches.stderr).trim()
        );
    }
    let branches = String::from_utf8(branches.stdout).context("Git branch name is not UTF-8")?;
    if let Some(branch) = branches.lines().find(|branch| !branch.is_empty()) {
        return Ok(branch.to_owned());
    }
    if let Some(commit) = resolve_git_commit(path, "HEAD")? {
        return Ok(commit);
    }
    if let Some(current) = current {
        return Ok(current);
    }
    Ok(DEFAULT_BASE_BRANCH.into())
}

fn symbolic_ref(path: &Path, reference: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["symbolic-ref", "--quiet", "--short", reference])
        .output()
        .with_context(|| format!("inspect Git reference {reference:?} at {}", path.display()))?;
    if output.status.success() {
        return String::from_utf8(output.stdout)
            .context("Git branch name is not UTF-8")
            .map(|branch| Some(branch.trim().to_owned()));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    bail!(
        "Could not inspect Git reference {reference:?} at {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn git_ref_resolves(path: &Path, reference: &str) -> Result<bool> {
    Ok(resolve_git_commit(path, reference)?.is_some())
}

fn resolve_git_commit(path: &Path, reference: &str) -> Result<Option<String>> {
    let commit = format!("{reference}^{{commit}}");
    let output = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["rev-parse", "--verify", "--quiet", &commit])
        .output()
        .with_context(|| format!("resolve Git reference {reference:?} at {}", path.display()))?;
    if output.status.success() {
        return String::from_utf8(output.stdout)
            .context("Git commit ID is not UTF-8")
            .map(|commit| Some(commit.trim().to_owned()));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    bail!(
        "Could not resolve Git reference {reference:?} at {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn ensure_initial_commit(path: &Path, base_branch: &str) -> Result<()> {
    let head = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .with_context(|| format!("inspect Git HEAD at {}", path.display()))?;
    if head.status.success() {
        return Ok(());
    }

    let symbolic_head = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .with_context(|| format!("inspect Git HEAD reference at {}", path.display()))?;
    if !symbolic_head.status.success() {
        bail!(
            "Could not inspect Git HEAD at {}: {}",
            path.display(),
            String::from_utf8_lossy(&head.stderr).trim()
        );
    }
    let refs = Command::new("git")
        .args(["-C"])
        .arg(path)
        .arg("show-ref")
        .output()
        .with_context(|| format!("inspect Git refs at {}", path.display()))?;
    if refs.status.success() {
        bail!(
            "Git HEAD at {} does not resolve, but the repository contains existing refs",
            path.display()
        );
    }
    if refs.status.code() != Some(1) {
        bail!(
            "Could not inspect Git refs at {}: {}",
            path.display(),
            String::from_utf8_lossy(&refs.stderr).trim()
        );
    }

    validate_branch_name(base_branch)?;
    select_head_branch(path, base_branch)?;

    let committed = Command::new("git")
        .args(["-C"])
        .arg(path)
        .env("GIT_AUTHOR_NAME", "Switchyard")
        .env("GIT_AUTHOR_EMAIL", "switchyard@localhost")
        .env("GIT_COMMITTER_NAME", "Switchyard")
        .env("GIT_COMMITTER_EMAIL", "switchyard@localhost")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args([
            "commit",
            "--allow-empty",
            "--only",
            "--no-gpg-sign",
            "-m",
            "Initial commit",
        ])
        .output()
        .with_context(|| format!("create initial commit at {}", path.display()))?;
    if !committed.status.success() {
        bail!(
            "Could not create the initial Git commit at {}: {}",
            path.display(),
            String::from_utf8_lossy(&committed.stderr).trim()
        );
    }
    Ok(())
}

fn validate_branch_name(branch: &str) -> Result<()> {
    let valid = Command::new("git")
        .args(["check-ref-format", "--branch", branch])
        .output()
        .context("validate Git branch")?;
    if !valid.status.success() {
        bail!("Invalid Git branch {branch:?}");
    }
    Ok(())
}

fn select_head_branch(path: &Path, branch: &str) -> Result<()> {
    let base_ref = format!("refs/heads/{branch}");
    let selected = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["symbolic-ref", "HEAD", &base_ref])
        .output()
        .with_context(|| format!("select Git branch {branch:?}"))?;
    if !selected.status.success() {
        bail!(
            "Could not select Git branch {branch:?}: {}",
            String::from_utf8_lossy(&selected.stderr).trim()
        );
    }
    Ok(())
}

fn has_git_metadata(path: &Path) -> Result<bool> {
    for directory in path.ancestors() {
        match fs::symlink_metadata(directory.join(".git")) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect existing Git metadata"),
        }
        let looks_bare = ["HEAD", "objects", "refs"]
            .iter()
            .map(|entry| directory.join(entry).try_exists())
            .collect::<io::Result<Vec<_>>>()?
            .into_iter()
            .all(|exists| exists);
        if looks_bare {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_branch_is_read_from_the_target_repository_context() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        fs::create_dir(&repository).unwrap();
        let initialized = Command::new("git")
            .args(["-C"])
            .arg(&repository)
            .arg("init")
            .output()
            .unwrap();
        assert!(initialized.status.success());
        let configured = Command::new("git")
            .args(["-C"])
            .arg(&repository)
            .args(["config", "init.defaultBranch", "trunk"])
            .output()
            .unwrap();
        assert!(configured.status.success());

        assert_eq!(preferred_initial_branch(&repository).unwrap(), "trunk");
    }

    #[test]
    fn temporary_branch_names_are_short_hex_tokens() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        initialize_test_repository(&repository);

        let plan = detached_worktree_plan(&repository, "main").unwrap();

        assert_eq!(plan.temporary_branch.len(), 10);
        assert!(
            plan.temporary_branch
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
    }

    #[test]
    fn temporary_branch_allocation_retries_with_another_short_token() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        initialize_test_repository(&repository);
        let seed = 42;
        let first = short_worktree_name(seed, 0);
        run_git(&repository, ["branch", &first, "main"]).unwrap();

        let plan = detached_worktree_plan_at(&repository, "main", seed).unwrap();

        assert_eq!(plan.temporary_branch, short_worktree_name(seed, 1));
        assert_ne!(plan.temporary_branch, first);
    }

    #[test]
    fn temporary_branch_cleanup_uses_the_branch_oid_after_detached_head_advances() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let checkout = root.path().join("checkout");
        fs::create_dir(&repository).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--initial-branch", "main"])
                .arg(&repository)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args([
                    "-c",
                    "user.name=Switchyard Test",
                    "-c",
                    "user.email=switchyard@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "initial",
                ])
                .status()
                .unwrap()
                .success()
        );
        let temporary_branch = "switchyard-session-test";
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["worktree", "add", "-b", temporary_branch])
                .arg(&checkout)
                .arg("main")
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(["checkout", "--detach"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args([
                    "-c",
                    "user.name=Switchyard Test",
                    "-c",
                    "user.email=switchyard@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "detached work",
                ])
                .status()
                .unwrap()
                .success()
        );

        detach_created_worktree(&repository, &checkout, temporary_branch).unwrap();

        assert!(
            !Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args([
                    "show-ref",
                    "--verify",
                    "--quiet",
                    "refs/heads/switchyard-session-test",
                ])
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn removes_a_clean_registered_worktree() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let checkout = root.path().join("checkout");
        initialize_test_repository(&repository);
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["worktree", "add", "--detach"])
                .arg(&checkout)
                .status()
                .unwrap()
                .success()
        );

        remove_worktree(&repository, &checkout).unwrap();

        assert!(!checkout.exists());
    }

    #[test]
    fn refuses_to_remove_a_dirty_registered_worktree() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let checkout = root.path().join("checkout");
        initialize_test_repository(&repository);
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["worktree", "add", "--detach"])
                .arg(&checkout)
                .status()
                .unwrap()
                .success()
        );
        fs::write(checkout.join("unsaved.txt"), "keep me").unwrap();

        assert!(remove_worktree(&repository, &checkout).is_err());
        assert!(checkout.exists());
    }

    #[test]
    fn refuses_to_remove_an_unreferenced_detached_commit() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let checkout = root.path().join("checkout");
        initialize_test_repository(&repository);
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["worktree", "add", "--detach"])
                .arg(&checkout)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args([
                    "-c",
                    "user.name=Switchyard Test",
                    "-c",
                    "user.email=switchyard@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "detached work",
                ])
                .status()
                .unwrap()
                .success()
        );

        let error = remove_worktree(&repository, &checkout).unwrap_err();

        assert!(format!("{error:#}").contains("not reachable from a branch or tag"));
        assert!(checkout.exists());
    }

    #[test]
    fn removes_stale_git_metadata_when_the_worktree_directory_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let checkout = root.path().join("checkout");
        initialize_test_repository(&repository);
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["worktree", "add", "--detach"])
                .arg(&checkout)
                .status()
                .unwrap()
                .success()
        );
        fs::remove_dir_all(&checkout).unwrap();

        remove_worktree(&repository, &checkout).unwrap();

        let worktrees = git_stdout(&repository, ["worktree", "list", "--porcelain"]).unwrap();
        assert!(
            !worktrees.contains(checkout.to_string_lossy().as_ref()),
            "{worktrees}"
        );
    }

    fn initialize_test_repository(repository: &Path) {
        fs::create_dir(repository).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--initial-branch", "main"])
                .arg(repository)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(repository)
                .args([
                    "-c",
                    "user.name=Switchyard Test",
                    "-c",
                    "user.email=switchyard@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "initial",
                ])
                .status()
                .unwrap()
                .success()
        );
    }
}
