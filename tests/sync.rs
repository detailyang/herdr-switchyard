use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Result, bail};
use herdr_switchyard::{
    coordinator::{Herdr, agent_name},
    model::{
        AgentSession, Config, CreatedAgentPane, OpenedWorkspace, Project, RuntimeAgent,
        RuntimeSnapshot, RuntimeWorkspace, Session, SessionMode,
    },
    picker::sync,
    store::Store,
};
use tempfile::tempdir;

struct SnapshotHerdr {
    snapshot: RuntimeSnapshot,
    entered: mpsc::Sender<()>,
    release: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
}

impl Herdr for SnapshotHerdr {
    fn snapshot(&self) -> Result<RuntimeSnapshot> {
        self.entered.send(()).unwrap();
        if let Some(release) = &self.release {
            release.lock().unwrap().recv().unwrap();
        }
        Ok(self.snapshot.clone())
    }

    fn runtime_namespace(&self) -> Option<String> {
        None
    }

    fn ensure_project_workspace(&self, _project: &Project) -> Result<String> {
        bail!("not used")
    }

    fn create_worktree(
        &self,
        _project: &Project,
        _source_workspace_id: &str,
        _session_name: &str,
    ) -> Result<herdr_switchyard::model::CreatedWorktree> {
        bail!("not used")
    }

    fn finish_detach(
        &self,
        _project: &Project,
        _session: &Session,
        _temporary_branch: &str,
    ) -> Result<()> {
        bail!("not used")
    }

    fn open_worktree(
        &self,
        _project: &Project,
        _source_workspace_id: &str,
        _session: &Session,
    ) -> Result<OpenedWorkspace> {
        bail!("not used")
    }

    fn open_local(&self, _project: &Project, _session: &Session) -> Result<OpenedWorkspace> {
        bail!("not used")
    }

    fn focus_workspace(&self, _workspace_id: &str) -> Result<()> {
        bail!("not used")
    }

    fn focus_agent(&self, _pane_id: &str) -> Result<()> {
        bail!("not used")
    }

    fn find_reusable_session_pane(
        &self,
        _workspace_id: &str,
        _session_name: &str,
        _cwd: &Path,
        _owned_tab_id: Option<&str>,
    ) -> Result<Option<CreatedAgentPane>> {
        bail!("not used")
    }

    fn create_agent_pane(
        &self,
        _workspace_id: &str,
        _session_name: &str,
        _cwd: &Path,
    ) -> Result<CreatedAgentPane> {
        bail!("not used")
    }

    fn close_tab(&self, _tab_id: &str) -> Result<()> {
        bail!("not used")
    }

    fn close_agent_tab(&self, _pane_id: &str) -> Result<()> {
        bail!("not used")
    }

    fn workspace_for_tab(&self, _tab_id: &str) -> Result<Option<String>> {
        bail!("not used")
    }

    fn close_workspace(&self, _workspace_id: &str) -> Result<()> {
        bail!("not used")
    }

    fn start_agent(
        &self,
        _name: &str,
        _kind: &str,
        _pane_id: &str,
        _args: &[String],
    ) -> Result<()> {
        bail!("not used")
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

fn snapshot(reference: &str) -> RuntimeSnapshot {
    RuntimeSnapshot {
        workspaces: vec![RuntimeWorkspace {
            id: "w1".into(),
            checkout_path: PathBuf::from("/worktrees/demo/feat-one"),
        }],
        agents: vec![RuntimeAgent {
            workspace_id: "w1".into(),
            pane_id: "w1:p1".into(),
            name: Some(agent_name(&project(), "feat/one")),
            kind: Some("codex".into()),
            status: "idle".into(),
            session: Some(AgentSession {
                agent: "codex".into(),
                kind: "id".into(),
                value: reference.into(),
            }),
        }],
    }
}

#[test]
fn concurrent_hooks_cannot_commit_an_older_snapshot_last() {
    let root = tempdir().unwrap();
    let store = Store::new(root.path().join("config"), root.path().join("state"));
    store
        .save_config(&Config {
            version: 1,
            ui: Default::default(),
            pins: Default::default(),
            projects: vec![project()],
        })
        .unwrap();
    store
        .update_state(|state| {
            state.sessions.push(Session {
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
            });
            Ok(())
        })
        .unwrap();

    let (old_entered_tx, old_entered_rx) = mpsc::channel();
    let (release_old_tx, release_old_rx) = mpsc::channel();
    let old = SnapshotHerdr {
        snapshot: snapshot("old-session"),
        entered: old_entered_tx,
        release: Some(Arc::new(Mutex::new(release_old_rx))),
    };
    let old_store = store.clone();
    let old_thread = thread::spawn(move || sync(&old_store, &old));
    old_entered_rx.recv().unwrap();

    let (new_entered_tx, _new_entered_rx) = mpsc::channel();
    let new = SnapshotHerdr {
        snapshot: snapshot("new-session"),
        entered: new_entered_tx,
        release: None,
    };
    let new_store = store.clone();
    let (new_done_tx, new_done_rx) = mpsc::channel();
    let new_thread = thread::spawn(move || {
        let result = sync(&new_store, &new);
        new_done_tx.send(()).unwrap();
        result
    });

    let _ = new_done_rx.recv_timeout(Duration::from_millis(200));
    release_old_tx.send(()).unwrap();
    old_thread.join().unwrap().unwrap();
    new_thread.join().unwrap().unwrap();

    assert_eq!(
        store.load_state().unwrap().sessions[0]
            .agent_session
            .as_ref()
            .unwrap()
            .value,
        "new-session"
    );
}
