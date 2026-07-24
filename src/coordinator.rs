use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::{
    model::{
        CreatedAgentPane, CreatedWorktree, OpenedWorkspace, Project, RuntimeSnapshot, Session,
        SessionMode, State, is_supported_agent,
    },
    paths::same_path,
    repository::{remove_worktree, validate_worktree_removal},
};

pub trait Herdr {
    fn snapshot(&self) -> Result<RuntimeSnapshot>;
    fn runtime_namespace(&self) -> Option<String>;
    fn ensure_project_workspace(&self, project: &Project) -> Result<String>;
    fn create_worktree(
        &self,
        project: &Project,
        source_workspace_id: &str,
        session_name: &str,
    ) -> Result<CreatedWorktree>;
    fn finish_detach(
        &self,
        project: &Project,
        session: &Session,
        temporary_branch: &str,
    ) -> Result<()>;
    fn open_worktree(
        &self,
        project: &Project,
        source_workspace_id: &str,
        session: &Session,
    ) -> Result<OpenedWorkspace>;
    fn open_local(&self, project: &Project, session: &Session) -> Result<OpenedWorkspace>;
    fn focus_workspace(&self, workspace_id: &str) -> Result<()>;
    fn focus_agent(&self, pane_id: &str) -> Result<()>;
    fn find_reusable_session_pane(
        &self,
        workspace_id: &str,
        session_name: &str,
        cwd: &Path,
        owned_tab_id: Option<&str>,
    ) -> Result<Option<CreatedAgentPane>>;
    fn create_agent_pane(
        &self,
        workspace_id: &str,
        session_name: &str,
        cwd: &Path,
    ) -> Result<CreatedAgentPane>;
    fn close_tab(&self, tab_id: &str) -> Result<()>;
    fn close_agent_tab(&self, pane_id: &str) -> Result<()>;
    fn workspace_for_tab(&self, tab_id: &str) -> Result<Option<String>>;
    fn close_workspace(&self, workspace_id: &str) -> Result<()>;
    fn start_agent(&self, name: &str, kind: &str, pane_id: &str, args: &[String]) -> Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    Focused,
    Opened,
    Created,
}

pub fn activate_existing<H: Herdr>(
    herdr: &H,
    project: &Project,
    state: &mut State,
    session_name: &str,
    now_ms: u64,
) -> Result<Activation> {
    ensure_supported_agent(project)?;
    let session_index = state
        .sessions
        .iter()
        .position(|session| session.project_id == project.id && session.name == session_name)
        .with_context(|| {
            format!(
                "session {session_name:?} is not registered for {}",
                project.name
            )
        })?;
    let recovered_pending_detach = if let Some(temporary_branch) = state.sessions[session_index]
        .pending_temporary_branch
        .clone()
    {
        let session = state.sessions[session_index].clone();
        herdr
            .finish_detach(project, &session, &temporary_branch)
            .with_context(|| format!("finish detaching session {session_name:?}"))?;
        true
    } else {
        false
    };
    let snapshot = herdr.snapshot().context("read Herdr state")?;
    sync_agent_sessions(state, &snapshot, std::slice::from_ref(project));

    let session = &mut state.sessions[session_index];
    session.last_used_at_ms = now_ms;

    if let Some(workspace) = snapshot
        .workspaces
        .iter()
        .find(|workspace| same_path(&workspace.checkout_path, &session.worktree_path))
    {
        if let Some(agent) = snapshot
            .agents
            .iter()
            .find(|agent| is_managed_agent(agent, project, session, &workspace.id))
        {
            herdr
                .focus_agent(&agent.pane_id)
                .with_context(|| format!("focus agent in session {session_name:?}"))?;
            if recovered_pending_detach {
                session.pending_temporary_branch = None;
            }
        } else {
            herdr
                .focus_workspace(&workspace.id)
                .with_context(|| format!("focus workspace for session {session_name:?}"))?;
            let runtime_namespace = herdr.runtime_namespace();
            let owned_tab_id = runtime_namespace
                .as_ref()
                .filter(|namespace| session.tab_namespace.as_ref() == Some(*namespace))
                .and(session.tab_id.as_deref());
            let reusable = herdr
                .find_reusable_session_pane(
                    &workspace.id,
                    &session.name,
                    &session.worktree_path,
                    owned_tab_id,
                )
                .with_context(|| format!("find existing tab for session {session_name:?}"))?;
            let (created, close_on_failure) = if let Some(pane) = reusable {
                (pane, false)
            } else {
                (
                    herdr.create_agent_pane(
                        &workspace.id,
                        &session.name,
                        &session.worktree_path,
                    )?,
                    true,
                )
            };
            let start_result = if recovered_pending_detach {
                start_new_agent(herdr, project, session, &created.pane_id)
            } else {
                start_resumed_agent(herdr, project, session, &created.pane_id)
            };
            if let Err(start_error) = start_result {
                if close_on_failure && let Err(close_error) = herdr.close_tab(&created.tab_id) {
                    return Err(start_error.context(format!(
                        "also failed to close new agent tab {}: {close_error:#}",
                        created.tab_id
                    )));
                }
                return Err(start_error);
            }
            if recovered_pending_detach {
                session.pending_temporary_branch = None;
            }
            session.tab_id = Some(created.tab_id.clone());
            session.tab_namespace = runtime_namespace;
            if !close_on_failure {
                herdr
                    .focus_agent(&created.pane_id)
                    .with_context(|| format!("focus agent in session {session_name:?}"))?;
            }
        }
        return Ok(Activation::Focused);
    }

    let opened = match session.mode {
        SessionMode::Local => herdr
            .open_local(project, session)
            .with_context(|| format!("open local session {session_name:?}"))?,
        SessionMode::Worktree => {
            let source_workspace_id = herdr
                .ensure_project_workspace(project)
                .with_context(|| format!("resolve project workspace for {}", project.name))?;
            herdr
                .open_worktree(project, &source_workspace_id, session)
                .with_context(|| format!("open worktree for session {session_name:?}"))?
        }
    };
    if recovered_pending_detach {
        start_new_agent(herdr, project, session, &opened.pane_id)?;
        session.pending_temporary_branch = None;
    } else {
        start_resumed_agent(herdr, project, session, &opened.pane_id)?;
    }
    session.tab_id = opened.tab_id;
    session.tab_namespace = herdr.runtime_namespace();
    Ok(Activation::Opened)
}

pub fn create_session<H: Herdr>(
    herdr: &H,
    project: &Project,
    state: &mut State,
    session_name: &str,
    mode: SessionMode,
    now_ms: u64,
) -> Result<Activation> {
    ensure_supported_agent(project)?;
    let session_name = session_name.trim();
    if session_name.is_empty() {
        bail!("session name cannot be empty");
    }
    if state
        .sessions
        .iter()
        .any(|session| session.project_id == project.id && session.name == session_name)
    {
        bail!(
            "session {session_name:?} already exists for {}",
            project.name
        );
    }

    if mode == SessionMode::Local {
        return create_local_session(herdr, project, state, session_name, now_ms);
    }

    let source_workspace_id = herdr
        .ensure_project_workspace(project)
        .with_context(|| format!("resolve project workspace for {}", project.name))?;
    let created = herdr
        .create_worktree(project, &source_workspace_id, session_name)
        .with_context(|| format!("create worktree for session {session_name:?}"))?;
    let detach_is_pending = created.pending_temporary_branch.is_some();
    state.sessions.push(Session {
        project_id: project.id.clone(),
        name: session_name.to_owned(),
        mode,
        worktree_path: created.workspace.worktree_path.clone(),
        pending_temporary_branch: created.pending_temporary_branch,
        created_at_ms: now_ms,
        last_used_at_ms: now_ms,
        agent_session: None,
        tab_id: created.workspace.tab_id.clone(),
        tab_namespace: herdr.runtime_namespace(),
    });

    if detach_is_pending && let Some(warning) = created.warning.as_deref() {
        bail!("session was created and registered, but {warning}");
    }
    start_new_agent(
        herdr,
        project,
        state.sessions.last().expect("session was just registered"),
        &created.workspace.pane_id,
    )?;
    if let Some(warning) = created.warning {
        bail!("session was created, registered, and started, but {warning}");
    }
    Ok(Activation::Created)
}

fn create_local_session<H: Herdr>(
    herdr: &H,
    project: &Project,
    state: &mut State,
    session_name: &str,
    now_ms: u64,
) -> Result<Activation> {
    let snapshot = herdr.snapshot().context("read Herdr state")?;
    state.sessions.push(Session {
        project_id: project.id.clone(),
        name: session_name.to_owned(),
        mode: SessionMode::Local,
        worktree_path: project.path.clone(),
        pending_temporary_branch: None,
        created_at_ms: now_ms,
        last_used_at_ms: now_ms,
        agent_session: None,
        tab_id: None,
        tab_namespace: None,
    });
    let session = state.sessions.last().expect("session was just registered");

    let tab_id = if let Some(workspace) = snapshot
        .workspaces
        .iter()
        .find(|workspace| same_path(&workspace.checkout_path, &project.path))
    {
        herdr
            .focus_workspace(&workspace.id)
            .with_context(|| format!("focus workspace for session {session_name:?}"))?;
        let created = herdr.create_agent_pane(&workspace.id, session_name, &project.path)?;
        if let Err(start_error) = start_new_agent(herdr, project, session, &created.pane_id) {
            if let Err(close_error) = herdr.close_tab(&created.tab_id) {
                return Err(start_error.context(format!(
                    "also failed to close new agent tab {}: {close_error:#}",
                    created.tab_id
                )));
            }
            return Err(start_error);
        }
        Some(created.tab_id)
    } else {
        let opened = herdr
            .open_local(project, session)
            .with_context(|| format!("open local session {session_name:?}"))?;
        start_new_agent(herdr, project, session, &opened.pane_id)?;
        opened.tab_id
    };
    state
        .sessions
        .last_mut()
        .expect("session was registered")
        .tab_id = tab_id;
    state
        .sessions
        .last_mut()
        .expect("session was registered")
        .tab_namespace = herdr.runtime_namespace();
    Ok(Activation::Created)
}

pub fn delete_session<H: Herdr>(
    herdr: &H,
    project: &Project,
    state: &mut State,
    session_name: &str,
) -> Result<()> {
    let session_index = state
        .sessions
        .iter()
        .position(|session| session.project_id == project.id && session.name == session_name)
        .with_context(|| {
            format!(
                "session {session_name:?} is not registered for {}",
                project.name
            )
        })?;
    let session = state.sessions[session_index].clone();
    let snapshot = herdr.snapshot().context("read Herdr state")?;
    match session.mode {
        SessionMode::Local => {
            let running_agent = snapshot.workspaces.iter().find_map(|workspace| {
                same_path(&workspace.checkout_path, &project.path).then(|| {
                    snapshot
                        .agents
                        .iter()
                        .find(|agent| is_managed_agent(agent, project, &session, &workspace.id))
                })?
            });
            if let Some(agent) = running_agent {
                herdr
                    .close_agent_tab(&agent.pane_id)
                    .with_context(|| format!("close Herdr tab for session {session_name:?}"))?;
            } else {
                let runtime_namespace = herdr.runtime_namespace();
                if let Some(tab_id) = runtime_namespace
                    .as_ref()
                    .filter(|namespace| session.tab_namespace.as_ref() == Some(*namespace))
                    .and(session.tab_id.as_deref())
                {
                    let workspace_id = herdr.workspace_for_tab(tab_id).with_context(|| {
                        format!("locate Herdr tab for session {session_name:?}")
                    })?;
                    let is_project_tab = workspace_id.as_deref().is_some_and(|workspace_id| {
                        snapshot.workspaces.iter().any(|workspace| {
                            workspace.id == workspace_id
                                && same_path(&workspace.checkout_path, &project.path)
                        })
                    });
                    if is_project_tab {
                        herdr.close_tab(tab_id).with_context(|| {
                            format!("close Herdr tab for session {session_name:?}")
                        })?;
                    }
                }
            }
        }
        SessionMode::Worktree => {
            validate_worktree_removal(&project.path, &session.worktree_path)
                .with_context(|| format!("validate worktree for session {session_name:?}"))?;
            let matching_workspaces = snapshot
                .workspaces
                .iter()
                .filter(|workspace| same_path(&workspace.checkout_path, &session.worktree_path))
                .collect::<Vec<_>>();
            if !matching_workspaces.is_empty() {
                let runtime_namespace = herdr.runtime_namespace();
                let owned_tab_id = runtime_namespace
                    .as_ref()
                    .filter(|namespace| session.tab_namespace.as_ref() == Some(*namespace))
                    .and(session.tab_id.as_deref());
                let workspace_from_tab = owned_tab_id
                    .map(|tab_id| herdr.workspace_for_tab(tab_id))
                    .transpose()
                    .with_context(|| {
                        format!("locate Herdr workspace for session {session_name:?}")
                    })?
                    .flatten();
                let workspace = workspace_from_tab
                    .as_deref()
                    .and_then(|workspace_id| {
                        matching_workspaces
                            .iter()
                            .find(|workspace| workspace.id == workspace_id)
                            .copied()
                    })
                    .or_else(|| {
                        matching_workspaces.iter().find_map(|workspace| {
                            snapshot
                                .agents
                                .iter()
                                .any(|agent| {
                                    is_managed_agent(agent, project, &session, &workspace.id)
                                })
                                .then_some(*workspace)
                        })
                    })
                    .or_else(|| (matching_workspaces.len() == 1).then_some(matching_workspaces[0]))
                    .with_context(|| {
                        format!(
                            "cannot safely close session {session_name:?}: its Herdr workspace ownership is ambiguous"
                        )
                    })?;
                herdr.close_workspace(&workspace.id).with_context(|| {
                    format!("close Herdr workspace for session {session_name:?}")
                })?;
            }
            remove_worktree(&project.path, &session.worktree_path)
                .with_context(|| format!("delete worktree for session {session_name:?}"))?;
        }
    }
    state.sessions.remove(session_index);
    Ok(())
}

pub fn sync_agent_sessions(
    state: &mut State,
    snapshot: &RuntimeSnapshot,
    projects: &[Project],
) -> bool {
    let mut changed = false;
    for session in &mut state.sessions {
        let Some(project) = projects
            .iter()
            .find(|project| project.id == session.project_id)
        else {
            continue;
        };
        let Some(workspace) = snapshot
            .workspaces
            .iter()
            .find(|workspace| same_path(&workspace.checkout_path, &session.worktree_path))
        else {
            continue;
        };
        let Some(agent_session) = snapshot
            .agents
            .iter()
            .find(|agent| is_managed_agent(agent, project, session, &workspace.id))
            .and_then(|agent| agent.session.clone())
        else {
            continue;
        };
        if session.agent_session.as_ref() != Some(&agent_session) {
            session.agent_session = Some(agent_session);
            changed = true;
        }
    }
    changed
}

fn start_new_agent<H: Herdr>(
    herdr: &H,
    project: &Project,
    session: &Session,
    pane_id: &str,
) -> Result<()> {
    herdr
        .start_agent(
            &agent_name(project, &session.name),
            &project.agent,
            pane_id,
            &project.agent_args,
        )
        .with_context(|| format!("start {} for session {:?}", project.agent, session.name))
}

fn start_resumed_agent<H: Herdr>(
    herdr: &H,
    project: &Project,
    session: &Session,
    pane_id: &str,
) -> Result<()> {
    let args = resume_args(project, session)?;
    herdr.start_agent(
        &agent_name(project, &session.name),
        &project.agent,
        pane_id,
        &args,
    )
}

fn resume_args(project: &Project, session: &Session) -> Result<Vec<String>> {
    let mut args = project.agent_args.clone();
    let agent_session = session
        .agent_session
        .as_ref()
        .filter(|session| session.agent == project.agent);
    match (project.agent.as_str(), agent_session) {
        ("codex", Some(session)) => args.extend(["resume".into(), session.value.clone()]),
        ("codex", None) => args.push("resume".into()),
        ("claude", Some(session)) => args.extend(["--resume".into(), session.value.clone()]),
        ("claude", None) => args.push("--resume".into()),
        ("pi", Some(session)) if session.kind == "path" && !session.value.is_empty() => {
            args.extend(["--session".into(), session.value.clone()]);
        }
        ("pi", _) => {}
        _ => bail!("unsupported agent {:?}", project.agent),
    }
    Ok(args)
}

fn ensure_supported_agent(project: &Project) -> Result<()> {
    if !is_supported_agent(&project.agent) {
        bail!(
            "unsupported agent {:?} for project {:?}",
            project.agent,
            project.name
        );
    }
    Ok(())
}

pub fn agent_name(project: &Project, session_name: &str) -> String {
    let raw = format!("{}-{session_name}", project.id);
    let mut name = String::with_capacity(raw.len());
    let mut previous_dash = false;
    for ch in raw.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if normalized == '-' && (name.is_empty() || previous_dash) {
            continue;
        }
        name.push(normalized);
        previous_dash = normalized == '-';
    }
    while name.ends_with('-') {
        name.pop();
    }
    let mut needs_hash = name != raw || name.len() > 32;
    if name.is_empty() || !name.starts_with(|ch: char| ch.is_ascii_lowercase()) {
        name.insert_str(0, "a-");
        needs_hash = true;
    }
    if !needs_hash {
        return name;
    }

    let suffix = format!("-{:08x}", stable_hash(raw.as_bytes()));
    let mut prefix = name.chars().take(32 - suffix.len()).collect::<String>();
    while prefix.ends_with('-') {
        prefix.pop();
    }
    prefix.push_str(&suffix);
    prefix
}

fn is_managed_agent(
    agent: &crate::model::RuntimeAgent,
    project: &Project,
    session: &Session,
    workspace_id: &str,
) -> bool {
    agent.workspace_id == workspace_id
        && agent.name.as_deref() == Some(agent_name(project, &session.name).as_str())
        && agent.kind.as_deref() == Some(project.agent.as_str())
}

fn stable_hash(input: &[u8]) -> u32 {
    input.iter().fold(0x811c_9dc5, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

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

    #[test]
    fn long_session_names_keep_unique_agent_names() {
        let first = agent_name(&project(), "feature/a-very-long-shared-session-prefix-one");
        let second = agent_name(&project(), "feature/a-very-long-shared-session-prefix-two");

        assert_ne!(first, second);
        assert!(first.len() <= 32);
        assert!(second.len() <= 32);
    }

    #[test]
    fn non_ascii_session_names_keep_unique_agent_names() {
        let first = agent_name(&project(), "功能一");
        let second = agent_name(&project(), "功能二");

        assert_ne!(first, second);
        assert!(first.len() <= 32);
        assert!(second.len() <= 32);
    }
}
