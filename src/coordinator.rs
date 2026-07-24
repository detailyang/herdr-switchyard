use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::{
    model::{
        CreatedAgentPane, CreatedWorktree, OpenedWorkspace, Project, RuntimeSnapshot, Session,
        State, is_supported_agent,
    },
    paths::same_path,
    repository::remove_worktree,
};

pub trait Herdr {
    fn snapshot(&self) -> Result<RuntimeSnapshot>;
    fn create_worktree(&self, project: &Project, session_name: &str) -> Result<CreatedWorktree>;
    fn finish_detach(
        &self,
        project: &Project,
        session: &Session,
        temporary_branch: &str,
    ) -> Result<()>;
    fn open_worktree(&self, project: &Project, session: &Session) -> Result<OpenedWorkspace>;
    fn focus_workspace(&self, workspace_id: &str) -> Result<()>;
    fn focus_agent(&self, pane_id: &str) -> Result<()>;
    fn create_agent_pane(
        &self,
        workspace_id: &str,
        session_name: &str,
        cwd: &Path,
    ) -> Result<CreatedAgentPane>;
    fn close_tab(&self, tab_id: &str) -> Result<()>;
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
            let created =
                herdr.create_agent_pane(&workspace.id, &session.name, &session.worktree_path)?;
            let start_result = if recovered_pending_detach {
                start_new_agent(herdr, project, session, &created.pane_id)
            } else {
                start_resumed_agent(herdr, project, session, &created.pane_id)
            };
            if let Err(start_error) = start_result {
                if let Err(close_error) = herdr.close_tab(&created.tab_id) {
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
        }
        return Ok(Activation::Focused);
    }

    let opened = herdr
        .open_worktree(project, session)
        .with_context(|| format!("open worktree for session {session_name:?}"))?;
    if recovered_pending_detach {
        start_new_agent(herdr, project, session, &opened.pane_id)?;
        session.pending_temporary_branch = None;
    } else {
        start_resumed_agent(herdr, project, session, &opened.pane_id)?;
    }
    Ok(Activation::Opened)
}

pub fn create_session<H: Herdr>(
    herdr: &H,
    project: &Project,
    state: &mut State,
    session_name: &str,
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

    let created = herdr
        .create_worktree(project, session_name)
        .with_context(|| format!("create worktree for session {session_name:?}"))?;
    state.sessions.push(Session {
        project_id: project.id.clone(),
        name: session_name.to_owned(),
        worktree_path: created.workspace.worktree_path.clone(),
        pending_temporary_branch: created.pending_temporary_branch,
        created_at_ms: now_ms,
        last_used_at_ms: now_ms,
        agent_session: None,
    });

    if let Some(warning) = created.warning {
        bail!("session was created and registered, but {warning}");
    }
    start_new_agent(
        herdr,
        project,
        state.sessions.last().expect("session was just registered"),
        &created.workspace.pane_id,
    )?;
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
    if snapshot
        .workspaces
        .iter()
        .any(|workspace| same_path(&workspace.checkout_path, &session.worktree_path))
    {
        bail!(
            "Session {session_name:?} is still open. Close its Herdr workspace first, then delete it."
        );
    }
    remove_worktree(&project.path, &session.worktree_path)
        .with_context(|| format!("delete worktree for session {session_name:?}"))?;
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
    let reference = session
        .agent_session
        .as_ref()
        .filter(|session| session.agent == project.agent)
        .map(|session| session.value.as_str());
    match (project.agent.as_str(), reference) {
        ("codex", Some(reference)) => args.extend(["resume".into(), reference.into()]),
        ("codex", None) => args.push("resume".into()),
        ("claude", Some(reference)) => args.extend(["--resume".into(), reference.into()]),
        ("claude", None) => args.push("--resume".into()),
        ("pi", Some(reference)) => args.extend(["--session".into(), reference.into()]),
        ("pi", None) => args.push("-r".into()),
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
