use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use herdr_switchyard::{
    coordinator::{
        Activation, Herdr, activate_existing, agent_name, create_session, delete_session,
        sync_agent_sessions,
    },
    model::{
        AgentSession, CreatedAgentPane, CreatedWorktree, OpenedWorkspace, Project, RuntimeAgent,
        RuntimeSnapshot, RuntimeWorkspace, Session, SessionMode, State,
    },
};

#[derive(Default)]
struct FakeHerdr {
    snapshot: RuntimeSnapshot,
    calls: RefCell<Vec<String>>,
    reusable_session_pane: Option<CreatedAgentPane>,
    runtime_namespace: Option<String>,
    tab_workspace: Option<String>,
    fail_start: bool,
    fail_close_tab: bool,
    fail_close_workspace: bool,
    create_warning: Option<String>,
    create_pending_detach: bool,
}

impl Herdr for FakeHerdr {
    fn snapshot(&self) -> Result<RuntimeSnapshot> {
        Ok(self.snapshot.clone())
    }

    fn runtime_namespace(&self) -> Option<String> {
        self.runtime_namespace.clone()
    }

    fn ensure_project_workspace(&self, project: &Project) -> Result<String> {
        self.calls
            .borrow_mut()
            .push(format!("ensure-project-workspace:{}", project.id));
        Ok("w-root".into())
    }

    fn create_worktree(
        &self,
        project: &Project,
        source_workspace_id: &str,
        session_name: &str,
    ) -> Result<CreatedWorktree> {
        self.calls.borrow_mut().push(format!(
            "create:{}:{source_workspace_id}:{session_name}",
            project.id
        ));
        Ok(CreatedWorktree {
            workspace: OpenedWorkspace {
                workspace_id: "w2".into(),
                pane_id: "w2:p1".into(),
                tab_id: Some("w2:t1".into()),
                worktree_path: PathBuf::from("/worktrees/demo/feat-one"),
            },
            pending_temporary_branch: self
                .create_pending_detach
                .then(|| "switchyard-session-pending".into()),
            warning: self.create_warning.clone(),
        })
    }

    fn finish_detach(
        &self,
        project: &Project,
        session: &Session,
        temporary_branch: &str,
    ) -> Result<()> {
        self.calls.borrow_mut().push(format!(
            "finish-detach:{}:{}:{temporary_branch}",
            project.id, session.name
        ));
        Ok(())
    }

    fn open_worktree(
        &self,
        project: &Project,
        source_workspace_id: &str,
        session: &Session,
    ) -> Result<OpenedWorkspace> {
        self.calls.borrow_mut().push(format!(
            "open:{}:{source_workspace_id}:{}",
            project.id, session.name
        ));
        Ok(OpenedWorkspace {
            workspace_id: "w2".into(),
            pane_id: "w2:p1".into(),
            tab_id: Some("w2:t1".into()),
            worktree_path: session.worktree_path.clone(),
        })
    }

    fn open_local(&self, project: &Project, session: &Session) -> Result<OpenedWorkspace> {
        self.calls
            .borrow_mut()
            .push(format!("open-local:{}:{}", project.id, session.name));
        Ok(OpenedWorkspace {
            workspace_id: "w2".into(),
            pane_id: "w2:p1".into(),
            tab_id: Some("w2:t1".into()),
            worktree_path: project.path.clone(),
        })
    }

    fn focus_workspace(&self, workspace_id: &str) -> Result<()> {
        self.calls
            .borrow_mut()
            .push(format!("focus:{workspace_id}"));
        Ok(())
    }

    fn focus_agent(&self, pane_id: &str) -> Result<()> {
        self.calls
            .borrow_mut()
            .push(format!("focus-agent:{pane_id}"));
        Ok(())
    }

    fn find_reusable_session_pane(
        &self,
        workspace_id: &str,
        session_name: &str,
        cwd: &Path,
        owned_tab_id: Option<&str>,
    ) -> Result<Option<CreatedAgentPane>> {
        if self.reusable_session_pane.is_some() {
            self.calls.borrow_mut().push(format!(
                "reuse-agent-pane:{workspace_id}:{session_name}:{}:{}",
                cwd.display(),
                owned_tab_id.unwrap_or("legacy")
            ));
        }
        Ok(self.reusable_session_pane.clone())
    }

    fn create_agent_pane(
        &self,
        workspace_id: &str,
        session_name: &str,
        cwd: &Path,
    ) -> Result<CreatedAgentPane> {
        self.calls.borrow_mut().push(format!(
            "create-agent-pane:{workspace_id}:{session_name}:{}",
            cwd.display()
        ));
        Ok(CreatedAgentPane {
            tab_id: format!("{workspace_id}:t4"),
            pane_id: format!("{workspace_id}:p4"),
        })
    }

    fn close_tab(&self, tab_id: &str) -> Result<()> {
        self.calls.borrow_mut().push(format!("close-tab:{tab_id}"));
        if self.fail_close_tab {
            anyhow::bail!("tab close failed");
        }
        Ok(())
    }

    fn close_agent_tab(&self, pane_id: &str) -> Result<()> {
        self.calls
            .borrow_mut()
            .push(format!("close-agent-tab:{pane_id}"));
        if self.fail_close_tab {
            anyhow::bail!("tab close failed");
        }
        Ok(())
    }

    fn workspace_for_tab(&self, tab_id: &str) -> Result<Option<String>> {
        self.calls
            .borrow_mut()
            .push(format!("workspace-for-tab:{tab_id}"));
        Ok(self.tab_workspace.clone())
    }

    fn close_workspace(&self, workspace_id: &str) -> Result<()> {
        self.calls
            .borrow_mut()
            .push(format!("close-workspace:{workspace_id}"));
        if self.fail_close_workspace {
            anyhow::bail!("workspace close failed");
        }
        Ok(())
    }

    fn start_agent(&self, name: &str, kind: &str, pane_id: &str, args: &[String]) -> Result<()> {
        self.calls
            .borrow_mut()
            .push(format!("start:{name}:{kind}:{pane_id}:{}", args.join(" ")));
        if self.fail_start {
            anyhow::bail!("agent start failed");
        }
        Ok(())
    }
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

fn session(path: impl AsRef<Path>) -> Session {
    Session {
        project_id: "demo".into(),
        name: "feat/one".into(),
        mode: SessionMode::Worktree,
        worktree_path: path.as_ref().to_owned(),
        pending_temporary_branch: None,
        created_at_ms: 1,
        last_used_at_ms: 1,
        agent_session: None,
        tab_id: None,
        tab_namespace: None,
    }
}

#[test]
fn focuses_the_agent_when_the_session_workspace_is_already_open() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: PathBuf::from("/worktrees/demo/feat-one"),
            }],
            agents: vec![RuntimeAgent {
                workspace_id: "w1".into(),
                pane_id: "w1:p3".into(),
                name: Some(agent_name(&project(), "feat/one")),
                kind: Some("codex".into()),
                status: "working".into(),
                session: None,
            }],
        },
        ..Default::default()
    };
    let mut state = State {
        sessions: vec![session("/worktrees/demo/feat-one")],
        ..Default::default()
    };

    let result = activate_existing(&herdr, &project(), &mut state, "feat/one", 42).unwrap();

    assert_eq!(result, Activation::Focused);
    assert_eq!(herdr.calls.into_inner(), ["focus-agent:w1:p3"]);
    assert_eq!(state.sessions[0].last_used_at_ms, 42);
}

#[test]
fn creates_a_worktree_records_it_and_starts_the_project_agent() {
    let herdr = FakeHerdr::default();
    let mut state = State::default();

    let result = create_session(
        &herdr,
        &project(),
        &mut state,
        "feat/one",
        SessionMode::Worktree,
        42,
    )
    .unwrap();

    assert_eq!(result, Activation::Created);
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.sessions[0].mode, SessionMode::Worktree);
    assert_eq!(state.sessions[0].tab_id.as_deref(), Some("w2:t1"));
    assert_eq!(
        herdr.calls.into_inner(),
        [
            "ensure-project-workspace:demo".to_owned(),
            "create:demo:w-root:feat/one".to_owned(),
            format!("start:{}:codex:w2:p1:", agent_name(&project(), "feat/one"))
        ]
    );
}

#[test]
fn creates_multiple_local_sessions_in_dedicated_tabs_of_the_project_workspace() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: project().path,
            }],
            agents: Vec::new(),
        },
        ..Default::default()
    };
    let mut state = State::default();

    create_session(
        &herdr,
        &project(),
        &mut state,
        "first",
        SessionMode::Local,
        42,
    )
    .unwrap();
    create_session(
        &herdr,
        &project(),
        &mut state,
        "second",
        SessionMode::Local,
        43,
    )
    .unwrap();

    assert_eq!(state.sessions.len(), 2);
    assert!(state.sessions.iter().all(
        |session| session.mode == SessionMode::Local && session.worktree_path == project().path
    ));
    assert_eq!(
        herdr.calls.into_inner(),
        [
            "focus:w1".to_owned(),
            "create-agent-pane:w1:first:/repos/demo".to_owned(),
            format!("start:{}:codex:w1:p4:", agent_name(&project(), "first")),
            "focus:w1".to_owned(),
            "create-agent-pane:w1:second:/repos/demo".to_owned(),
            format!("start:{}:codex:w1:p4:", agent_name(&project(), "second")),
        ]
    );
}

#[test]
fn opens_a_dormant_local_session_in_the_project_directory() {
    let herdr = FakeHerdr::default();
    let mut local = session("/repos/demo");
    local.mode = SessionMode::Local;
    let mut state = State {
        sessions: vec![local],
        ..Default::default()
    };

    let result = activate_existing(&herdr, &project(), &mut state, "feat/one", 42).unwrap();

    assert_eq!(result, Activation::Opened);
    assert_eq!(
        herdr.calls.into_inner(),
        [
            "open-local:demo:feat/one".to_owned(),
            format!(
                "start:{}:codex:w2:p1:resume",
                agent_name(&project(), "feat/one")
            ),
        ]
    );
}

#[test]
fn deletes_an_idle_local_session_without_removing_the_shared_project_directory() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: project().path,
            }],
            agents: vec![RuntimeAgent {
                workspace_id: "w1".into(),
                pane_id: "w1:p2".into(),
                name: Some(agent_name(&project(), "another")),
                kind: Some("codex".into()),
                status: "idle".into(),
                session: None,
            }],
        },
        ..Default::default()
    };
    let mut local = session("/repos/demo");
    local.mode = SessionMode::Local;
    let mut state = State {
        sessions: vec![local],
        ..Default::default()
    };

    delete_session(&herdr, &project(), &mut state, "feat/one").unwrap();

    assert!(state.sessions.is_empty());
    assert!(herdr.calls.into_inner().is_empty());
}

#[test]
fn closes_an_owned_dormant_local_session_tab_before_deleting_its_record() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: project().path,
            }],
            agents: Vec::new(),
        },
        runtime_namespace: Some("/sockets/default".into()),
        tab_workspace: Some("w1".into()),
        ..Default::default()
    };
    let mut local = session("/repos/demo");
    local.mode = SessionMode::Local;
    local.tab_id = Some("w1:t2".into());
    local.tab_namespace = Some("/sockets/default".into());
    let mut state = State {
        sessions: vec![local],
        ..Default::default()
    };

    delete_session(&herdr, &project(), &mut state, "feat/one").unwrap();

    assert!(state.sessions.is_empty());
    assert_eq!(
        herdr.calls.into_inner(),
        ["workspace-for-tab:w1:t2", "close-tab:w1:t2"]
    );
}

#[test]
fn deletes_a_dormant_local_session_when_its_saved_tab_is_already_gone() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: project().path,
            }],
            agents: Vec::new(),
        },
        runtime_namespace: Some("/sockets/default".into()),
        ..Default::default()
    };
    let mut local = session("/repos/demo");
    local.mode = SessionMode::Local;
    local.tab_id = Some("w1:t2".into());
    local.tab_namespace = Some("/sockets/default".into());
    let mut state = State {
        sessions: vec![local],
        ..Default::default()
    };

    delete_session(&herdr, &project(), &mut state, "feat/one").unwrap();

    assert!(state.sessions.is_empty());
    assert_eq!(herdr.calls.into_inner(), ["workspace-for-tab:w1:t2"]);
}

#[test]
fn closes_the_exact_agent_tab_before_deleting_a_running_local_session() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: project().path,
            }],
            agents: vec![RuntimeAgent {
                workspace_id: "w1".into(),
                pane_id: "w1:p2".into(),
                name: Some(agent_name(&project(), "feat/one")),
                kind: Some("codex".into()),
                status: "idle".into(),
                session: None,
            }],
        },
        ..Default::default()
    };
    let mut local = session("/repos/demo");
    local.mode = SessionMode::Local;
    let mut state = State {
        sessions: vec![local],
        ..Default::default()
    };

    delete_session(&herdr, &project(), &mut state, "feat/one").unwrap();

    assert!(state.sessions.is_empty());
    assert_eq!(herdr.calls.into_inner(), ["close-agent-tab:w1:p2"]);
}

#[test]
fn closes_the_exact_agent_tab_when_saved_tab_ownership_is_stale() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: project().path,
            }],
            agents: vec![RuntimeAgent {
                workspace_id: "w1".into(),
                pane_id: "w1:p2".into(),
                name: Some(agent_name(&project(), "feat/one")),
                kind: Some("codex".into()),
                status: "idle".into(),
                session: None,
            }],
        },
        runtime_namespace: Some("/sockets/current".into()),
        ..Default::default()
    };
    let mut local = session("/repos/demo");
    local.mode = SessionMode::Local;
    local.tab_id = Some("w1:t2".into());
    local.tab_namespace = Some("/sockets/other".into());
    let mut state = State {
        sessions: vec![local],
        ..Default::default()
    };

    delete_session(&herdr, &project(), &mut state, "feat/one").unwrap();

    assert!(state.sessions.is_empty());
    assert_eq!(herdr.calls.into_inner(), ["close-agent-tab:w1:p2"]);
}

#[test]
fn keeps_a_local_session_when_closing_its_agent_tab_fails() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: project().path,
            }],
            agents: vec![RuntimeAgent {
                workspace_id: "w1".into(),
                pane_id: "w1:p2".into(),
                name: Some(agent_name(&project(), "feat/one")),
                kind: Some("codex".into()),
                status: "idle".into(),
                session: None,
            }],
        },
        fail_close_tab: true,
        ..Default::default()
    };
    let mut local = session("/repos/demo");
    local.mode = SessionMode::Local;
    let mut state = State {
        sessions: vec![local],
        ..Default::default()
    };

    let error = delete_session(&herdr, &project(), &mut state, "feat/one").unwrap_err();

    assert!(format!("{error:#}").contains("close Herdr tab"));
    assert_eq!(state.sessions.len(), 1);
}

#[test]
fn closes_the_workspace_before_deleting_an_open_worktree_session() {
    let (_root, repository, checkout) = repository_with_worktree();
    let mut project = project();
    project.path = repository;
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w2".into(),
                checkout_path: checkout.clone(),
            }],
            agents: Vec::new(),
        },
        runtime_namespace: Some("/sockets/default".into()),
        ..Default::default()
    };
    let mut stored = session(&checkout);
    stored.tab_id = Some("stale:t1".into());
    stored.tab_namespace = Some("/sockets/default".into());
    let mut state = State {
        sessions: vec![stored],
        ..Default::default()
    };

    delete_session(&herdr, &project, &mut state, "feat/one").unwrap();

    assert_eq!(
        herdr.calls.into_inner(),
        ["workspace-for-tab:stale:t1", "close-workspace:w2"]
    );
    assert!(state.sessions.is_empty());
    assert!(!checkout.exists());
}

#[test]
fn closes_the_owned_workspace_when_multiple_workspaces_share_the_worktree_path() {
    let (_root, repository, checkout) = repository_with_worktree();
    let mut project = project();
    project.path = repository;
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![
                RuntimeWorkspace {
                    id: "unrelated".into(),
                    checkout_path: checkout.clone(),
                },
                RuntimeWorkspace {
                    id: "owned".into(),
                    checkout_path: checkout.clone(),
                },
            ],
            agents: Vec::new(),
        },
        runtime_namespace: Some("/sockets/default".into()),
        tab_workspace: Some("owned".into()),
        ..Default::default()
    };
    let mut stored = session(&checkout);
    stored.tab_id = Some("owned:t1".into());
    stored.tab_namespace = Some("/sockets/default".into());
    let mut state = State {
        sessions: vec![stored],
        ..Default::default()
    };

    delete_session(&herdr, &project, &mut state, "feat/one").unwrap();

    assert_eq!(
        herdr.calls.into_inner(),
        ["workspace-for-tab:owned:t1", "close-workspace:owned"]
    );
    assert!(state.sessions.is_empty());
}

#[test]
fn keeps_a_worktree_session_when_closing_its_workspace_fails() {
    let (_root, repository, checkout) = repository_with_worktree();
    let mut project = project();
    project.path = repository;
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w2".into(),
                checkout_path: checkout.clone(),
            }],
            agents: Vec::new(),
        },
        fail_close_workspace: true,
        ..Default::default()
    };
    let mut state = State {
        sessions: vec![session(&checkout)],
        ..Default::default()
    };

    let error = delete_session(&herdr, &project, &mut state, "feat/one").unwrap_err();

    assert!(format!("{error:#}").contains("close Herdr workspace"));
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(herdr.calls.into_inner(), ["close-workspace:w2"]);
}

#[test]
fn does_not_close_an_open_workspace_when_its_worktree_is_dirty() {
    let (_root, repository, checkout) = repository_with_worktree();
    std::fs::write(checkout.join("unsaved.txt"), "keep me").unwrap();
    let mut project = project();
    project.path = repository;
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w2".into(),
                checkout_path: checkout.clone(),
            }],
            agents: Vec::new(),
        },
        ..Default::default()
    };
    let mut state = State {
        sessions: vec![session(&checkout)],
        ..Default::default()
    };

    let error = delete_session(&herdr, &project, &mut state, "feat/one").unwrap_err();

    assert!(format!("{error:#}").contains("uncommitted changes"));
    assert!(herdr.calls.into_inner().is_empty());
    assert_eq!(state.sessions.len(), 1);
    assert!(checkout.exists());
}

fn repository_with_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("repository");
    let checkout = root.path().join("checkout");
    initialize_repository(&repository);
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
    (root, repository, checkout)
}

fn initialize_repository(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
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
            .arg("-C")
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
fn registers_a_created_worktree_before_reporting_a_detach_warning() {
    let herdr = FakeHerdr {
        create_warning: Some("temporary branch remains".into()),
        create_pending_detach: true,
        ..Default::default()
    };
    let mut state = State::default();

    let error = create_session(
        &herdr,
        &project(),
        &mut state,
        "Improve login",
        SessionMode::Worktree,
        42,
    )
    .unwrap_err();

    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.sessions[0].name, "Improve login");
    assert_eq!(
        state.sessions[0].pending_temporary_branch.as_deref(),
        Some("switchyard-session-pending")
    );
    assert!(error.to_string().contains("created and registered"));
    assert!(error.to_string().contains("temporary branch remains"));
    assert!(
        !herdr
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("start:"))
    );
}

#[test]
fn starts_and_registers_the_agent_before_reporting_a_tab_label_warning() {
    let herdr = FakeHerdr {
        create_warning: Some("could not rename its root tab".into()),
        ..Default::default()
    };
    let mut state = State::default();

    let error = create_session(
        &herdr,
        &project(),
        &mut state,
        "Short title",
        SessionMode::Worktree,
        42,
    )
    .unwrap_err();

    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.sessions[0].name, "Short title");
    assert!(error.to_string().contains("could not rename its root tab"));
    assert!(
        herdr
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("start:"))
    );
}

#[test]
fn retries_a_pending_detach_before_starting_a_new_agent() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: PathBuf::from("/worktrees/demo/feat-one"),
            }],
            agents: Vec::new(),
        },
        ..Default::default()
    };
    let mut pending = session("/worktrees/demo/feat-one");
    pending.pending_temporary_branch = Some("switchyard-session-pending".into());
    let mut state = State {
        sessions: vec![pending],
        ..Default::default()
    };

    let result = activate_existing(&herdr, &project(), &mut state, "feat/one", 42).unwrap();

    assert_eq!(result, Activation::Focused);
    assert_eq!(state.sessions[0].pending_temporary_branch, None);
    assert_eq!(
        herdr.calls.into_inner(),
        [
            "finish-detach:demo:feat/one:switchyard-session-pending".to_owned(),
            "focus:w1".to_owned(),
            "create-agent-pane:w1:feat/one:/worktrees/demo/feat-one".to_owned(),
            format!("start:{}:codex:w1:p4:", agent_name(&project(), "feat/one")),
        ]
    );
}

#[test]
fn keeps_pending_detach_until_the_initial_agent_starts() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: PathBuf::from("/worktrees/demo/feat-one"),
            }],
            agents: Vec::new(),
        },
        fail_start: true,
        ..Default::default()
    };
    let mut pending = session("/worktrees/demo/feat-one");
    pending.pending_temporary_branch = Some("switchyard-session-pending".into());
    let mut state = State {
        sessions: vec![pending],
        ..Default::default()
    };

    let error = activate_existing(&herdr, &project(), &mut state, "feat/one", 42).unwrap_err();

    assert!(format!("{error:#}").contains("agent start failed"));
    assert_eq!(
        state.sessions[0].pending_temporary_branch.as_deref(),
        Some("switchyard-session-pending")
    );
}

#[test]
fn opens_a_dormant_worktree_and_resumes_its_exact_agent_session() {
    let herdr = FakeHerdr::default();
    let mut stored = session("/worktrees/demo/feat-one");
    stored.agent_session = Some(AgentSession {
        agent: "codex".into(),
        kind: "id".into(),
        value: "session-123".into(),
    });
    let mut state = State {
        sessions: vec![stored],
        ..Default::default()
    };

    let result = activate_existing(&herdr, &project(), &mut state, "feat/one", 42).unwrap();

    assert_eq!(result, Activation::Opened);
    assert_eq!(
        herdr.calls.into_inner(),
        [
            "ensure-project-workspace:demo".to_owned(),
            "open:demo:w-root:feat/one".to_owned(),
            format!(
                "start:{}:codex:w2:p1:resume session-123",
                agent_name(&project(), "feat/one")
            )
        ]
    );
}

#[test]
fn opens_a_dormant_pi_session_without_history_as_a_new_session() {
    let herdr = FakeHerdr::default();
    let mut pi_project = project();
    pi_project.agent = "pi".into();
    let mut state = State {
        sessions: vec![session("/worktrees/demo/feat-one")],
        ..Default::default()
    };

    let result = activate_existing(&herdr, &pi_project, &mut state, "feat/one", 42).unwrap();

    assert_eq!(result, Activation::Opened);
    assert_eq!(
        herdr.calls.into_inner(),
        [
            "ensure-project-workspace:demo".to_owned(),
            "open:demo:w-root:feat/one".to_owned(),
            format!("start:{}:pi:w2:p1:", agent_name(&pi_project, "feat/one"))
        ]
    );
}

#[test]
fn opens_a_dormant_pi_session_with_its_exact_history() {
    let herdr = FakeHerdr::default();
    let mut pi_project = project();
    pi_project.agent = "pi".into();
    let mut stored = session("/worktrees/demo/feat-one");
    stored.agent_session = Some(AgentSession {
        agent: "pi".into(),
        kind: "path".into(),
        value: "/sessions/pi.jsonl".into(),
    });
    let mut state = State {
        sessions: vec![stored],
        ..Default::default()
    };

    let result = activate_existing(&herdr, &pi_project, &mut state, "feat/one", 42).unwrap();

    assert_eq!(result, Activation::Opened);
    assert_eq!(
        herdr.calls.into_inner(),
        [
            "ensure-project-workspace:demo".to_owned(),
            "open:demo:w-root:feat/one".to_owned(),
            format!(
                "start:{}:pi:w2:p1:--session /sessions/pi.jsonl",
                agent_name(&pi_project, "feat/one")
            )
        ]
    );
}

#[test]
fn opens_a_dormant_pi_session_with_a_non_path_reference_as_new() {
    let herdr = FakeHerdr::default();
    let mut pi_project = project();
    pi_project.agent = "pi".into();
    let mut stored = session("/worktrees/demo/feat-one");
    stored.agent_session = Some(AgentSession {
        agent: "pi".into(),
        kind: "id".into(),
        value: "not-a-session-path".into(),
    });
    let mut state = State {
        sessions: vec![stored],
        ..Default::default()
    };

    activate_existing(&herdr, &pi_project, &mut state, "feat/one", 42).unwrap();

    assert_eq!(
        herdr.calls.into_inner().last().unwrap(),
        &format!("start:{}:pi:w2:p1:", agent_name(&pi_project, "feat/one"))
    );
}

#[test]
fn focuses_the_project_agent_when_a_workspace_has_multiple_agents() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: PathBuf::from("/worktrees/demo/feat-one"),
            }],
            agents: vec![
                RuntimeAgent {
                    workspace_id: "w1".into(),
                    pane_id: "w1:p2".into(),
                    name: Some("other-agent".into()),
                    kind: Some("claude".into()),
                    status: "working".into(),
                    session: Some(AgentSession {
                        agent: "claude".into(),
                        kind: "id".into(),
                        value: "claude-123".into(),
                    }),
                },
                RuntimeAgent {
                    workspace_id: "w1".into(),
                    pane_id: "w1:p3".into(),
                    name: Some(agent_name(&project(), "feat/one")),
                    kind: Some("codex".into()),
                    status: "idle".into(),
                    session: Some(AgentSession {
                        agent: "codex".into(),
                        kind: "id".into(),
                        value: "codex-123".into(),
                    }),
                },
            ],
        },
        ..Default::default()
    };
    let mut state = State {
        sessions: vec![session("/worktrees/demo/feat-one")],
        ..Default::default()
    };

    activate_existing(&herdr, &project(), &mut state, "feat/one", 42).unwrap();

    assert_eq!(herdr.calls.into_inner(), ["focus-agent:w1:p3"]);
}

#[test]
fn sync_uses_the_project_agent_session_when_a_workspace_has_multiple_agents() {
    let mut state = State {
        sessions: vec![session("/worktrees/demo/feat-one")],
        ..Default::default()
    };
    let snapshot = RuntimeSnapshot {
        workspaces: vec![RuntimeWorkspace {
            id: "w1".into(),
            checkout_path: PathBuf::from("/worktrees/demo/feat-one"),
        }],
        agents: vec![
            RuntimeAgent {
                workspace_id: "w1".into(),
                pane_id: "w1:p2".into(),
                name: Some("other-agent".into()),
                kind: Some("claude".into()),
                status: "working".into(),
                session: Some(AgentSession {
                    agent: "claude".into(),
                    kind: "id".into(),
                    value: "claude-123".into(),
                }),
            },
            RuntimeAgent {
                workspace_id: "w1".into(),
                pane_id: "w1:p3".into(),
                name: Some(agent_name(&project(), "feat/one")),
                kind: Some("codex".into()),
                status: "idle".into(),
                session: Some(AgentSession {
                    agent: "codex".into(),
                    kind: "id".into(),
                    value: "codex-123".into(),
                }),
            },
        ],
    };

    sync_agent_sessions(&mut state, &snapshot, &[project()]);

    assert_eq!(
        state.sessions[0].agent_session.as_ref().unwrap().value,
        "codex-123"
    );
}

#[test]
fn creates_a_dedicated_tab_when_only_an_unrelated_agent_is_running() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: PathBuf::from("/worktrees/demo/feat-one"),
            }],
            agents: vec![RuntimeAgent {
                workspace_id: "w1".into(),
                pane_id: "w1:p2".into(),
                name: Some("other-agent".into()),
                kind: Some("claude".into()),
                status: "working".into(),
                session: None,
            }],
        },
        ..Default::default()
    };
    let mut state = State {
        sessions: vec![session("/worktrees/demo/feat-one")],
        ..Default::default()
    };

    activate_existing(&herdr, &project(), &mut state, "feat/one", 42).unwrap();

    assert_eq!(state.sessions[0].tab_id.as_deref(), Some("w1:t4"));
    assert_eq!(
        herdr.calls.into_inner(),
        [
            "focus:w1".to_owned(),
            "create-agent-pane:w1:feat/one:/worktrees/demo/feat-one".to_owned(),
            format!(
                "start:{}:codex:w1:p4:resume",
                agent_name(&project(), "feat/one")
            )
        ]
    );
}

#[test]
fn reuses_an_existing_session_tab_when_its_agent_has_exited() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: PathBuf::from("/worktrees/demo/feat-one"),
            }],
            agents: Vec::new(),
        },
        reusable_session_pane: Some(CreatedAgentPane {
            tab_id: "w1:t3".into(),
            pane_id: "w1:p3".into(),
        }),
        runtime_namespace: Some("/sockets/default".into()),
        ..Default::default()
    };
    let mut stored = session("/worktrees/demo/feat-one");
    stored.tab_id = Some("w1:t3".into());
    stored.tab_namespace = Some("/sockets/default".into());
    let mut state = State {
        sessions: vec![stored],
        ..Default::default()
    };

    activate_existing(&herdr, &project(), &mut state, "feat/one", 42).unwrap();

    assert_eq!(state.sessions[0].tab_id.as_deref(), Some("w1:t3"));
    assert_eq!(
        herdr.calls.into_inner(),
        [
            "focus:w1".to_owned(),
            "reuse-agent-pane:w1:feat/one:/worktrees/demo/feat-one:w1:t3".to_owned(),
            format!(
                "start:{}:codex:w1:p3:resume",
                agent_name(&project(), "feat/one")
            ),
            "focus-agent:w1:p3".to_owned(),
        ]
    );
}

#[test]
fn does_not_trust_a_tab_id_from_another_herdr_runtime() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: PathBuf::from("/worktrees/demo/feat-one"),
            }],
            agents: Vec::new(),
        },
        reusable_session_pane: Some(CreatedAgentPane {
            tab_id: "w1:t3".into(),
            pane_id: "w1:p3".into(),
        }),
        runtime_namespace: Some("/sockets/current".into()),
        ..Default::default()
    };
    let mut stored = session("/worktrees/demo/feat-one");
    stored.tab_id = Some("w1:t9".into());
    stored.tab_namespace = Some("/sockets/other".into());
    let mut state = State {
        sessions: vec![stored],
        ..Default::default()
    };

    activate_existing(&herdr, &project(), &mut state, "feat/one", 42).unwrap();

    assert!(
        herdr
            .calls
            .borrow()
            .iter()
            .any(|call| call.ends_with(":legacy"))
    );
    assert_eq!(state.sessions[0].tab_id.as_deref(), Some("w1:t3"));
    assert_eq!(
        state.sessions[0].tab_namespace.as_deref(),
        Some("/sockets/current")
    );
}

#[test]
fn does_not_close_a_reused_session_tab_when_agent_start_fails() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: PathBuf::from("/worktrees/demo/feat-one"),
            }],
            agents: Vec::new(),
        },
        reusable_session_pane: Some(CreatedAgentPane {
            tab_id: "w1:t3".into(),
            pane_id: "w1:p3".into(),
        }),
        fail_start: true,
        ..Default::default()
    };
    let mut state = State {
        sessions: vec![session("/worktrees/demo/feat-one")],
        ..Default::default()
    };

    let error = activate_existing(&herdr, &project(), &mut state, "feat/one", 42).unwrap_err();

    assert!(error.to_string().contains("agent start failed"));
    assert!(
        !herdr
            .calls
            .into_inner()
            .iter()
            .any(|call| call.starts_with("close-tab:"))
    );
}

#[test]
fn closes_a_new_agent_tab_when_resume_start_fails() {
    let herdr = FakeHerdr {
        snapshot: RuntimeSnapshot {
            workspaces: vec![RuntimeWorkspace {
                id: "w1".into(),
                checkout_path: PathBuf::from("/worktrees/demo/feat-one"),
            }],
            agents: Vec::new(),
        },
        fail_start: true,
        ..Default::default()
    };
    let mut state = State {
        sessions: vec![session("/worktrees/demo/feat-one")],
        ..Default::default()
    };

    let error = activate_existing(&herdr, &project(), &mut state, "feat/one", 42).unwrap_err();

    assert!(error.to_string().contains("agent start failed"));
    assert_eq!(
        herdr.calls.into_inner(),
        [
            "focus:w1".to_owned(),
            "create-agent-pane:w1:feat/one:/worktrees/demo/feat-one".to_owned(),
            format!(
                "start:{}:codex:w1:p4:resume",
                agent_name(&project(), "feat/one")
            ),
            "close-tab:w1:t4".to_owned(),
        ]
    );
}
