use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const SUPPORTED_AGENTS: &[&str] = &["codex", "claude", "pi"];
pub const DEFAULT_BASE_BRANCH: &str = "main";

pub fn is_supported_agent(agent: &str) -> bool {
    SUPPORTED_AGENTS.contains(&agent)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub pins: PinConfig,
    #[serde(default)]
    pub projects: Vec<Project>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            ui: UiConfig::default(),
            pins: PinConfig::default(),
            projects: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PinConfig {
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub sessions: Vec<PinnedSession>,
}

impl PinConfig {
    pub fn project_is_pinned(&self, project_id: &str) -> bool {
        self.projects.iter().any(|id| id == project_id)
    }

    pub fn session_is_pinned(&self, project_id: &str, session_name: &str) -> bool {
        self.sessions
            .iter()
            .any(|session| session.project_id == project_id && session.session_name == session_name)
    }

    pub fn toggle_project(&mut self, project_id: &str) {
        if self.project_is_pinned(project_id) {
            self.projects.retain(|id| id != project_id);
        } else {
            self.projects.push(project_id.to_owned());
        }
    }

    pub fn toggle_session(&mut self, project_id: &str, session_name: &str) {
        if self.session_is_pinned(project_id, session_name) {
            self.sessions.retain(|session| {
                session.project_id != project_id || session.session_name != session_name
            });
        } else {
            self.sessions.push(PinnedSession {
                project_id: project_id.to_owned(),
                session_name: session_name.to_owned(),
            });
        }
    }

    pub fn remove_project(&mut self, project_id: &str) {
        self.projects.retain(|id| id != project_id);
        self.sessions
            .retain(|session| session.project_id != project_id);
    }

    pub fn remove_session(&mut self, project_id: &str, session_name: &str) {
        self.sessions.retain(|session| {
            session.project_id != project_id || session.session_name != session_name
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedSession {
    pub project_id: String,
    pub session_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    #[default]
    JadeDark,
    MidnightDark,
    PaperLight,
    SandLight,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UiConfig {
    #[serde(default)]
    pub theme: ThemeName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub agent: String,
    #[serde(default = "default_base_branch")]
    pub base_branch: String,
    #[serde(default)]
    pub agent_args: Vec<String>,
}

fn default_base_branch() -> String {
    DEFAULT_BASE_BRANCH.to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    #[serde(default)]
    pub sessions: Vec<Session>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            sessions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub project_id: String,
    pub name: String,
    #[serde(default)]
    pub mode: SessionMode,
    pub worktree_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_temporary_branch: Option<String>,
    pub created_at_ms: u64,
    pub last_used_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<AgentSession>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_namespace: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SessionMode {
    Local,
    #[default]
    Worktree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    pub agent: String,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeWorkspace {
    pub id: String,
    pub checkout_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAgent {
    pub workspace_id: String,
    pub pane_id: String,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub status: String,
    pub session: Option<AgentSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeSnapshot {
    pub workspaces: Vec<RuntimeWorkspace>,
    pub agents: Vec<RuntimeAgent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedWorkspace {
    pub workspace_id: String,
    pub pane_id: String,
    pub tab_id: Option<String>,
    pub worktree_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedWorktree {
    pub workspace: OpenedWorkspace,
    pub pending_temporary_branch: Option<String>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedAgentPane {
    pub tab_id: String,
    pub pane_id: String,
}
