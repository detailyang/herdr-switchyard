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
    model::{Project, Session, SessionMode},
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
    printf '%s\n' '{{"result":{{"type":"workspace_list","workspaces":[{{"workspace_id":"w1","label":"feat-one","worktree":{{"checkout_path":"/worktrees/demo/feat-one"}}}},{{"workspace_id":"w4","label":"remote-lap"}}]}}}}'
    ;;
  "pane list")
    printf '%s\n' '{{"result":{{"type":"pane_list","panes":[{{"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1","cwd":"/worktrees/demo/feat-one","agent":"codex"}},{{"pane_id":"w1:p3","workspace_id":"w1","tab_id":"w1:t3","cwd":"/worktrees/demo/feat-one"}},{{"pane_id":"w1:p4","workspace_id":"w1","tab_id":"w1:t4","cwd":"/worktrees/demo/feat-one"}},{{"pane_id":"w4:p1","workspace_id":"w4","tab_id":"w4:t1","cwd":"/repos/demo"}}]}}}}'
    ;;
  "tab list")
    printf '%s\n' '{{"result":{{"type":"tab_list","tabs":[{{"tab_id":"w1:t1","workspace_id":"w1","label":"feat/one","number":1,"focused":false}},{{"tab_id":"w1:t3","workspace_id":"w1","label":"feat/one","number":3,"focused":false}},{{"tab_id":"w1:t4","workspace_id":"w1","label":"feat/one","number":4,"focused":true}}]}}}}'
    ;;
  "pane process-info")
    if [ "$4" = 'w1:p4' ]; then foreground=999; else foreground=123; fi
    printf '%s\n' "{{\"result\":{{\"type\":\"pane_process_info\",\"process_info\":{{\"pane_id\":\"$4\",\"shell_pid\":123,\"foreground_process_group_id\":$foreground}}}}}}"
    ;;
  "agent list")
    printf '%s\n' '{{"result":{{"type":"agent_list","agents":[{{"workspace_id":"w1","pane_id":"w1:p2","name":"{managed_name}","agent":"codex","agent_status":"working","agent_session":{{"agent":"codex","kind":"id","value":"session-123"}}}}]}}}}'
    ;;
  "worktree create")
    printf '%s\n' '{{"result":{{"type":"worktree_created","workspace":{{"workspace_id":"w2"}},"root_pane":{{"pane_id":"w2:p1"}},"worktree":{{"path":"/worktrees/demo/feat-two"}}}}}}'
    ;;
  "worktree open")
    printf '%s\n' '{{"result":{{"type":"worktree_opened","workspace":{{"workspace_id":"w3"}},"tab":{{"tab_id":"w3:t1"}},"root_pane":{{"pane_id":"w3:p1"}},"worktree":{{"path":"/worktrees/demo/feat-one"}}}}}}'
    ;;
  "workspace create")
    case " $* " in
      *" --json "*) printf '%s\n' '{{"error":{{"message":"unknown option: --json"}}}}' >&2; exit 2 ;;
    esac
    printf '%s\n' '{{"result":{{"type":"workspace_created","workspace":{{"workspace_id":"w5"}},"root_pane":{{"pane_id":"w5:p1"}}}}}}'
    ;;
  "pane get")
    case "$3" in
      w2:p1) tab_id='w2:t1' ;;
      w4:p1) tab_id='w4:t1' ;;
      w5:p1) tab_id='w5:t1' ;;
      *) tab_id='w1:t1' ;;
    esac
    printf '%s\n' "{{\"result\":{{\"type\":\"pane_info\",\"pane\":{{\"pane_id\":\"$3\",\"tab_id\":\"$tab_id\"}}}}}}"
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
    assert_eq!(snapshot.workspaces[1].id, "w4");
    assert_eq!(
        snapshot.workspaces[1].checkout_path,
        PathBuf::from("/repos/demo")
    );
    assert_eq!(snapshot.agents[0].pane_id, "w1:p2");
    assert_eq!(
        snapshot.agents[0].session.as_ref().unwrap().value,
        "session-123"
    );
}

#[test]
fn creates_a_named_project_workspace_instead_of_reusing_an_unrelated_workspace_at_the_same_path() {
    let (_root, herdr, log) = fake_herdr();

    let workspace_id = herdr.ensure_project_workspace(&project()).unwrap();

    assert_eq!(workspace_id, "w5");
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("workspace create --cwd /repos/demo --label Demo --no-focus"));
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
if [ "$1 $2" = "tab rename" ]; then
  printf '%s\n' '{{"result":{{"type":"tab_renamed"}}}}'
  exit 0
fi
cwd='{}'
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
printf '%s\n' '{{"result":{{"type":"worktree_created","workspace":{{"workspace_id":"w2"}},"tab":{{"tab_id":"w2:t1"}},"root_pane":{{"pane_id":"w2:p1"}},"worktree":{{"path":"{}"}}}}}}'
"#,
        log.display(),
        repository.display(),
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
        .create_worktree(&configured_project, "w-root", "Improve login flow")
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
    let temporary_prefix = format!("{}-", std::process::id());
    let temporary_ref_prefix = format!("refs/heads/{temporary_prefix}");
    let temporary_branches = Command::new("git")
        .args(["-C"])
        .arg(repository)
        .args(["for-each-ref", "--format=%(refname)", &temporary_ref_prefix])
        .output()
        .unwrap();
    assert!(temporary_branches.status.success());
    assert!(temporary_branches.stdout.is_empty());
    let temporary_config_pattern = format!("^branch\\.{}-", std::process::id());
    let temporary_branch_config = Command::new("git")
        .args(["-C"])
        .arg(&configured_project.path)
        .args(["config", "--get-regexp", &temporary_config_pattern])
        .output()
        .unwrap();
    assert_eq!(temporary_branch_config.status.code(), Some(1));
    assert!(temporary_branch_config.stdout.is_empty());

    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("worktree create --workspace w-root"));
    assert!(calls.contains(&format!("--branch {temporary_prefix}")));
    assert!(calls.contains(&format!(
        "--base {base_commit} --label Improve login flow --focus --json"
    )));
    assert!(calls.contains("tab rename w2:t1 Improve login flow"));
    assert!(!calls.contains("--branch Improve login flow"));
}

#[test]
fn returns_the_created_workspace_with_a_warning_when_detaching_fails() {
    let (root, herdr, log) = fake_herdr();
    let repository = root.path().join("demo");
    initialize_repository(&repository);
    let mut configured_project = project();
    configured_project.path = repository;

    let created = herdr
        .create_worktree(&configured_project, "w-root", "Improve login flow")
        .unwrap();

    assert_eq!(created.workspace.workspace_id, "w2");
    assert!(
        created
            .warning
            .as_deref()
            .unwrap()
            .contains("could not finish detaching")
    );
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("pane get w2:p1"));
    assert!(calls.contains("tab rename w2:t1 Improve login flow"));
    assert!(
        created
            .pending_temporary_branch
            .as_deref()
            .unwrap()
            .starts_with(&format!("{}-", std::process::id()))
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
if [ "$1 $2" = 'pane get' ]; then
  printf '%s\n' '{{"result":{{"type":"pane_info","pane":{{"pane_id":"w2:p1","tab_id":"w2:t1"}}}}}}'
  exit 0
fi
if [ "$1 $2" = 'tab rename' ]; then
  printf '%s\n' '{{"result":{{"type":"tab_renamed"}}}}'
  exit 0
fi
if [ "$1 $2" = 'worktree open' ]; then
  printf '%s\n' '{{"result":{{"type":"worktree_opened","workspace":{{"workspace_id":"w2"}},"root_pane":{{"pane_id":"w2:p1"}},"worktree":{{"path":"{}"}}}}}}'
  exit 0
fi
cwd='{}'
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
        repository.display(),
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
        .create_worktree(&configured_project, "w-root", "Recover response")
        .unwrap();

    assert_eq!(created.warning, None);
    assert_eq!(
        fs::canonicalize(&created.workspace.worktree_path).unwrap(),
        fs::canonicalize(checkout).unwrap()
    );
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("worktree open --workspace w-root"));
    assert!(calls.contains("--label Recover response --focus --json"));
    assert!(calls.contains("pane get w2:p1"));
    assert!(calls.contains("tab rename w2:t1 Recover response"));
}

#[test]
fn a_tab_lookup_failure_does_not_reopen_an_already_created_worktree() {
    let root = tempdir().unwrap();
    let repository = root.path().join("demo");
    let checkout = root.path().join("checkout");
    initialize_repository(&repository);
    let executable = root.path().join("herdr");
    let log = root.path().join("calls.log");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$1 $2" = 'pane get' ]; then
  printf '%s\n' '{{"error":{{"message":"pane unavailable"}}}}' >&2
  exit 1
fi
if [ "$1 $2" = 'worktree open' ]; then
  printf '%s\n' '{{"result":{{"type":"worktree_opened","workspace":{{"workspace_id":"w2"}},"root_pane":{{"pane_id":"w2:p1"}},"worktree":{{"path":"{}"}}}}}}'
  exit 0
fi
cwd='{}'
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
        repository.display(),
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
        .create_worktree(&configured_project, "w-root", "Short title")
        .unwrap();

    assert_eq!(created.pending_temporary_branch, None);
    assert!(
        created
            .warning
            .as_deref()
            .unwrap()
            .contains("pane unavailable")
    );
    let calls = fs::read_to_string(log).unwrap();
    assert!(!calls.contains("worktree open"));
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
cwd='{}'
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
        repository.display(),
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
        .create_worktree(&configured_project, "w-root", "Recover response")
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
        mode: SessionMode::Worktree,
        worktree_path: PathBuf::from("/worktrees/demo/feat-one"),
        pending_temporary_branch: None,
        created_at_ms: 1,
        last_used_at_ms: 1,
        agent_session: None,
        tab_id: None,
        tab_namespace: None,
    };

    let opened = herdr.open_worktree(&project(), "w-root", &session).unwrap();

    assert_eq!(opened.workspace_id, "w3");
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains(
        "worktree open --workspace w-root --path /worktrees/demo/feat-one --label feat/one --focus --json"
    ));
    assert!(calls.contains("tab rename w3:t1 feat/one"));
}

#[test]
fn opens_a_local_session_in_the_configured_project_directory() {
    let (_root, herdr, log) = fake_herdr();
    let session = Session {
        project_id: "demo".into(),
        name: "local one".into(),
        mode: SessionMode::Local,
        worktree_path: PathBuf::from("/repos/demo"),
        pending_temporary_branch: None,
        created_at_ms: 1,
        last_used_at_ms: 1,
        agent_session: None,
        tab_id: None,
        tab_namespace: None,
    };

    let opened = herdr.open_local(&project(), &session).unwrap();

    assert_eq!(opened.workspace_id, "w5");
    assert_eq!(opened.worktree_path, PathBuf::from("/repos/demo"));
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("workspace create --cwd /repos/demo --label Demo --focus"));
    assert!(!calls.contains("workspace create --cwd /repos/demo --label Demo --focus --json"));
    assert!(calls.contains("pane get w5:p1"));
    assert!(calls.contains("tab rename w5:t1 local one"));
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
fn skips_a_busy_focused_tab_and_reuses_an_available_shell() {
    let (_root, herdr, log) = fake_herdr();

    let pane = herdr
        .find_reusable_session_pane(
            "w1",
            "feat/one",
            std::path::Path::new("/worktrees/demo/feat-one"),
            None,
        )
        .unwrap()
        .unwrap();

    assert_eq!(pane.tab_id, "w1:t3");
    assert_eq!(pane.pane_id, "w1:p3");
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("tab list --workspace w1"));
    assert!(calls.contains("pane list --workspace w1"));
}

#[test]
fn does_not_substitute_an_unowned_tab_for_a_recorded_tab() {
    let (_root, herdr, _log) = fake_herdr();

    let pane = herdr
        .find_reusable_session_pane(
            "w1",
            "feat/one",
            std::path::Path::new("/worktrees/demo/feat-one"),
            Some("w1:t1"),
        )
        .unwrap();

    assert_eq!(pane, None);
}

#[test]
fn closes_a_dedicated_agent_tab() {
    let (_root, herdr, log) = fake_herdr();

    herdr.close_tab("w1:t4").unwrap();

    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("tab close w1:t4"));
}

#[test]
fn closes_a_workspace() {
    let (_root, herdr, log) = fake_herdr();

    herdr.close_workspace("w1").unwrap();

    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("workspace close w1"));
}

#[test]
fn closes_the_tab_containing_an_exact_agent_pane() {
    let (_root, herdr, log) = fake_herdr();

    herdr.close_agent_tab("w1:p2").unwrap();

    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("pane get w1:p2"));
    assert!(calls.contains("tab close w1:t1"));
}

#[test]
fn resolves_the_workspace_containing_a_tab() {
    let (_root, herdr, log) = fake_herdr();

    let workspace_id = herdr.workspace_for_tab("w1:t3").unwrap();

    assert_eq!(workspace_id.as_deref(), Some("w1"));
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("tab list"));
}
