#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use herdr_switchyard::{
    coordinator::{Herdr, agent_name},
    herdr::CliHerdr,
    model::{Project, Session},
};
use tempfile::tempdir;

fn fake_herdr() -> (tempfile::TempDir, CliHerdr, PathBuf) {
    let root = tempdir().unwrap();
    let executable = root.path().join("herdr");
    let log = root.path().join("calls.log");
    let managed_name = agent_name(&project(), "feat/one");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$1 $2" in
  "workspace list")
    printf '%s\n' '{{"result":{{"type":"workspace_list","workspaces":[{{"workspace_id":"w1","worktree":{{"checkout_path":"/worktrees/demo/feat-one"}}}}]}}}}'
    ;;
  "agent list")
    printf '%s\n' '{{"result":{{"type":"agent_list","agents":[{{"workspace_id":"w1","pane_id":"w1:p2","name":"{managed_name}","agent":"codex","agent_status":"working","agent_session":{{"agent":"codex","kind":"id","value":"session-123"}}}}]}}}}'
    ;;
  "worktree create")
    printf '%s\n' '{{"result":{{"type":"worktree_created","workspace":{{"workspace_id":"w2"}},"root_pane":{{"pane_id":"w2:p1"}},"worktree":{{"path":"/worktrees/demo/feat-two"}}}}}}'
    ;;
  "worktree open")
    printf '%s\n' '{{"result":{{"type":"worktree_opened","workspace":{{"workspace_id":"w3"}},"root_pane":{{"pane_id":"w3:p1"}},"worktree":{{"path":"/worktrees/demo/feat-one"}}}}}}'
    ;;
  "tab create")
    printf '%s\n' '{{"result":{{"type":"tab_created","tab":{{"tab_id":"w1:t4"}},"root_pane":{{"pane_id":"w1:p4"}}}}}}'
    ;;
  *) printf '%s\n' '{{"result":{{"type":"ok"}}}}' ;;
esac
"#,
        log.display()
    );
    fs::write(&executable, script).unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    let client = CliHerdr::new(&executable);
    (root, client, log)
}

fn project() -> Project {
    Project {
        id: "demo".into(),
        name: "Demo".into(),
        path: PathBuf::from("/repos/demo"),
        agent: "codex".into(),
        base_branch: "main".into(),
        agent_args: Vec::new(),
    }
}

fn initialize_repository(path: &Path) {
    fs::create_dir_all(path).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "--initial-branch", "main"])
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["-C"])
            .arg(path)
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

#[test]
fn parses_the_runtime_snapshot_from_herdr_json() {
    let (_root, herdr, _log) = fake_herdr();

    let snapshot = herdr.snapshot().unwrap();

    assert_eq!(snapshot.workspaces[0].id, "w1");
    assert_eq!(snapshot.agents[0].pane_id, "w1:p2");
    assert_eq!(
        snapshot.agents[0].session.as_ref().unwrap().value,
        "session-123"
    );
}

#[test]
fn creates_a_focused_detached_worktree_without_persisting_a_session_branch() {
    let root = tempdir().unwrap();
    let repository = root.path().join("demo");
    let checkout = root.path().join("checkout");
    initialize_repository(&repository);
    assert!(
        Command::new("git")
            .args(["-C"])
            .arg(&repository)
            .args(["branch", "switchyard"])
            .status()
            .unwrap()
            .success()
    );
    let base_commit = Command::new("git")
        .args(["-C"])
        .arg(&repository)
        .args(["rev-parse", "main"])
        .output()
        .unwrap();
    assert!(base_commit.status.success());
    let base_commit = String::from_utf8(base_commit.stdout).unwrap();
    let base_commit = base_commit.trim();

    let executable = root.path().join("herdr");
    let log = root.path().join("calls.log");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
cwd=''
branch=''
base=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --cwd) cwd="$2"; shift 2 ;;
    --branch) branch="$2"; shift 2 ;;
    --base) base="$2"; shift 2 ;;
    *) shift ;;
  esac
done
git -C "$cwd" worktree add -b "$branch" '{}' "$base" >/dev/null 2>&1 || exit 1
printf '%s\n' '{{"result":{{"type":"worktree_created","workspace":{{"workspace_id":"w2"}},"root_pane":{{"pane_id":"w2:p1"}},"worktree":{{"path":"{}"}}}}}}'
"#,
        log.display(),
        checkout.display(),
        checkout.display(),
    );
    fs::write(&executable, script).unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    let herdr = CliHerdr::new(&executable);
    let mut configured_project = project();
    configured_project.path = repository.clone();

    let opened = herdr
        .create_worktree(&configured_project, "Improve login flow")
        .unwrap();

    assert_eq!(opened.workspace.workspace_id, "w2");
    assert_eq!(opened.workspace.pane_id, "w2:p1");
    assert_eq!(opened.workspace.worktree_path, checkout);
    assert_eq!(opened.pending_temporary_branch, None);
    assert_eq!(opened.warning, None);
    assert!(
        !Command::new("git")
            .args(["-C"])
            .arg(&opened.workspace.worktree_path)
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .status()
            .unwrap()
            .success()
    );
    let temporary_branches = Command::new("git")
        .args(["-C"])
        .arg(repository)
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/switchyard-session-",
        ])
        .output()
        .unwrap();
    assert!(temporary_branches.status.success());
    assert!(temporary_branches.stdout.is_empty());
    let temporary_branch_config = Command::new("git")
        .args(["-C"])
        .arg(&configured_project.path)
        .args(["config", "--get-regexp", "^branch\\.switchyard-session-"])
        .output()
        .unwrap();
    assert_eq!(temporary_branch_config.status.code(), Some(1));
    assert!(temporary_branch_config.stdout.is_empty());

    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("worktree create --cwd"));
    assert!(calls.contains("--branch switchyard-session-"));
    assert!(calls.contains(&format!(
        "--base {base_commit} --label Improve login flow --focus --json"
    )));
    assert!(!calls.contains("--branch Improve login flow"));
}

#[test]
fn returns_the_created_workspace_with_a_warning_when_detaching_fails() {
    let (root, herdr, _log) = fake_herdr();
    let repository = root.path().join("demo");
    initialize_repository(&repository);
    let mut configured_project = project();
    configured_project.path = repository;

    let created = herdr
        .create_worktree(&configured_project, "Improve login flow")
        .unwrap();

    assert_eq!(created.workspace.workspace_id, "w2");
    assert!(
        created
            .warning
            .as_deref()
            .unwrap()
            .contains("could not finish detaching")
    );
    assert!(
        created
            .pending_temporary_branch
            .as_deref()
            .unwrap()
            .starts_with("switchyard-session-")
    );
}

#[test]
fn recovers_a_created_worktree_when_herdr_returns_invalid_create_json() {
    let root = tempdir().unwrap();
    let repository = root.path().join("demo");
    let checkout = root.path().join("checkout");
    initialize_repository(&repository);
    let executable = root.path().join("herdr");
    let log = root.path().join("calls.log");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$1 $2" = 'worktree open' ]; then
  printf '%s\n' '{{"result":{{"type":"worktree_opened","workspace":{{"workspace_id":"w2"}},"root_pane":{{"pane_id":"w2:p1"}},"worktree":{{"path":"{}"}}}}}}'
  exit 0
fi
cwd=''
branch=''
base=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --cwd) cwd="$2"; shift 2 ;;
    --branch) branch="$2"; shift 2 ;;
    --base) base="$2"; shift 2 ;;
    *) shift ;;
  esac
done
git -C "$cwd" worktree add -b "$branch" '{}' "$base" >/dev/null 2>&1 || exit 1
printf '%s\n' '{{invalid-json'
"#,
        log.display(),
        checkout.display(),
        checkout.display(),
    );
    fs::write(&executable, script).unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    let herdr = CliHerdr::new(&executable);
    let mut configured_project = project();
    configured_project.path = repository;

    let created = herdr
        .create_worktree(&configured_project, "Recover response")
        .unwrap();

    assert_eq!(created.warning, None);
    assert_eq!(
        fs::canonicalize(&created.workspace.worktree_path).unwrap(),
        fs::canonicalize(checkout).unwrap()
    );
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("worktree open --cwd"));
    assert!(calls.contains("--label Recover response --focus --json"));
}

#[test]
fn returns_a_pending_session_when_herdr_recovery_json_is_also_invalid() {
    let root = tempdir().unwrap();
    let repository = root.path().join("demo");
    let checkout = root.path().join("checkout");
    initialize_repository(&repository);
    let executable = root.path().join("herdr");
    let script = format!(
        r#"#!/bin/sh
if [ "$1 $2" = 'worktree open' ]; then
  printf '%s\n' '{{still-invalid'
  exit 0
fi
cwd=''
branch=''
base=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --cwd) cwd="$2"; shift 2 ;;
    --branch) branch="$2"; shift 2 ;;
    --base) base="$2"; shift 2 ;;
    *) shift ;;
  esac
done
git -C "$cwd" worktree add -b "$branch" '{}' "$base" >/dev/null 2>&1 || exit 1
printf '%s\n' '{{invalid-create'
"#,
        checkout.display(),
    );
    fs::write(&executable, script).unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    let herdr = CliHerdr::new(&executable);
    let mut configured_project = project();
    configured_project.path = repository;

    let created = herdr
        .create_worktree(&configured_project, "Recover response")
        .unwrap();

    assert_eq!(
        fs::canonicalize(&created.workspace.worktree_path).unwrap(),
        fs::canonicalize(checkout).unwrap()
    );
    assert!(created.workspace.workspace_id.is_empty());
    assert!(created.pending_temporary_branch.is_some());
    assert!(
        created
            .warning
            .as_deref()
            .unwrap()
            .contains("could not recover its Herdr workspace")
    );
}

#[test]
fn opens_a_registered_worktree_by_its_persisted_path() {
    let (_root, herdr, log) = fake_herdr();
    let session = Session {
        project_id: "demo".into(),
        name: "feat/one".into(),
        worktree_path: PathBuf::from("/worktrees/demo/feat-one"),
        pending_temporary_branch: None,
        created_at_ms: 1,
        last_used_at_ms: 1,
        agent_session: None,
    };

    let opened = herdr.open_worktree(&project(), &session).unwrap();

    assert_eq!(opened.workspace_id, "w3");
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains(
        "worktree open --cwd /repos/demo --path /worktrees/demo/feat-one --label feat/one --focus --json"
    ));
}

#[test]
fn creates_a_dedicated_agent_tab_in_an_open_workspace() {
    let (_root, herdr, log) = fake_herdr();

    let created = herdr
        .create_agent_pane(
            "w1",
            "feat/one",
            std::path::Path::new("/worktrees/demo/feat-one"),
        )
        .unwrap();

    assert_eq!(created.tab_id, "w1:t4");
    assert_eq!(created.pane_id, "w1:p4");
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains(
        "tab create --workspace w1 --cwd /worktrees/demo/feat-one --label feat/one --focus"
    ));
}

#[test]
fn closes_a_dedicated_agent_tab() {
    let (_root, herdr, log) = fake_herdr();

    herdr.close_tab("w1:t4").unwrap();

    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("tab close w1:t4"));
}
