use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};

use anyhow::Result;
use herdr_switchyard::{
    coordinator::{
        Activation, Herdr, activate_existing, agent_name, create_session, delete_session,
        sync_agent_sessions,
    },
    model::{
        AgentSession, CreatedAgentPane, CreatedWorktree, OpenedWorkspace, Project, RuntimeAgent,
        RuntimeSnapshot, RuntimeWorkspace, Session, State,
    },
};

#[derive(Default)]
struct FakeHerdr {
    snapshot: RuntimeSnapshot,
    calls: RefCell<Vec<String>>,
    fail_start: bool,
    create_warning: Option<String>,
}

impl Herdr for FakeHerdr {
    fn snapshot(&self) -> Result<RuntimeSnapshot> {
        Ok(self.snapshot.clone())
    }

    fn create_worktree(&self, project: &Project, session_name: &str) -> Result<CreatedWorktree> {
        self.calls
            .borrow_mut()
            .push(format!("create:{}:{session_name}", project.id));
        Ok(CreatedWorktree {
            workspace: OpenedWorkspace {
                workspace_id: "w2".into(),
                pane_id: "w2:p1".into(),
                worktree_path: PathBuf::from("/worktrees/demo/feat-one"),
            },
            pending_temporary_branch: self
                .create_warning
                .as_ref()
                .map(|_| "switchyard-session-pending".into()),
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

    fn open_worktree(&self, project: &Project, session: &Session) -> Result<OpenedWorkspace> {
        self.calls
            .borrow_mut()
            .push(format!("open:{}:{}", project.id, session.name));
        Ok(OpenedWorkspace {
            workspace_id: "w2".into(),
            pane_id: "w2:p1".into(),
            worktree_path: session.worktree_path.clone(),
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
        worktree_path: path.as_ref().to_owned(),
        pending_temporary_branch: None,
        created_at_ms: 1,
        last_used_at_ms: 1,
        agent_session: None,
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

    let result = create_session(&herdr, &project(), &mut state, "feat/one", 42).unwrap();

    assert_eq!(result, Activation::Created);
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(
        herdr.calls.into_inner(),
        [
            "create:demo:feat/one".to_owned(),
            format!("start:{}:codex:w2:p1:", agent_name(&project(), "feat/one"))
        ]
    );
}

#[test]
fn refuses_to_delete_an_open_session() {
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
    let mut state = State {
        sessions: vec![session("/worktrees/demo/feat-one")],
        ..Default::default()
    };

    let error = delete_session(&herdr, &project(), &mut state, "feat/one").unwrap_err();

    assert!(format!("{error:#}").contains("Close its Herdr workspace first"));
    assert_eq!(state.sessions.len(), 1);
    assert!(herdr.calls.into_inner().is_empty());
}

#[test]
fn registers_a_created_worktree_before_reporting_a_detach_warning() {
    let herdr = FakeHerdr {
        create_warning: Some("temporary branch remains".into()),
        ..Default::default()
    };
    let mut state = State::default();

    let error = create_session(&herdr, &project(), &mut state, "Improve login", 42).unwrap_err();

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
            "open:demo:feat/one".to_owned(),
            format!(
                "start:{}:codex:w2:p1:resume session-123",
                agent_name(&project(), "feat/one")
            )
        ]
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
