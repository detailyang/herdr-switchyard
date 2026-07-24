use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::{
    coordinator::Herdr,
    model::{
        AgentSession, CreatedAgentPane, CreatedWorktree, OpenedWorkspace, Project, RuntimeAgent,
        RuntimeSnapshot, RuntimeWorkspace, Session,
    },
    paths::same_path,
    repository::{detach_created_worktree, detached_worktree_plan, worktree_path_for_branch},
};

#[derive(Debug, Clone)]
pub struct CliHerdr {
    executable: PathBuf,
    runtime_namespace: Option<String>,
}

impl CliHerdr {
    pub fn from_environment() -> Self {
        let mut client = Self::new(
            std::env::var_os("HERDR_BIN_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("herdr")),
        );
        client.runtime_namespace = std::env::var("HERDR_SOCKET_PATH").ok();
        client
    }

    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            runtime_namespace: None,
        }
    }

    pub fn open_picker(&self) -> Result<()> {
        self.run_json([
            "plugin",
            "pane",
            "open",
            "--plugin",
            "herdr.switchyard",
            "--entrypoint",
            "picker",
            "--placement",
            "popup",
            "--focus",
        ])?;
        Ok(())
    }

    fn run_json<I, S>(&self, args: I) -> Result<Value>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        serde_json::from_slice(&self.run(args)?).context("parse Herdr JSON response")
    }

    fn run<I, S>(&self, args: I) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new(&self.executable)
            .args(args)
            .output()
            .with_context(|| format!("run {}", self.executable.display()))?;
        if !output.status.success() {
            let message = serde_json::from_slice::<Value>(&output.stderr)
                .ok()
                .and_then(|value| value.pointer("/error/message")?.as_str().map(str::to_owned))
                .unwrap_or_else(|| String::from_utf8_lossy(&output.stderr).trim().to_owned());
            bail!("Herdr command failed: {message}");
        }
        Ok(output.stdout)
    }
}

impl Herdr for CliHerdr {
    fn snapshot(&self) -> Result<RuntimeSnapshot> {
        let workspace_response = self.run_json(["workspace", "list"])?;
        let pane_response = self.run_json(["pane", "list"])?;
        let panes = array_at(&pane_response, "/result/panes")?;
        let workspaces = array_at(&workspace_response, "/result/workspaces")?
            .iter()
            .filter_map(|workspace| {
                let id = string_at(workspace, "/workspace_id").ok()?;
                let checkout_path = workspace
                    .pointer("/worktree/checkout_path")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        panes.iter().find_map(|pane| {
                            (pane.pointer("/workspace_id").and_then(Value::as_str) == Some(id))
                                .then(|| pane.pointer("/cwd").and_then(Value::as_str))
                                .flatten()
                        })
                    })?;
                Some(RuntimeWorkspace {
                    id: id.to_owned(),
                    checkout_path: PathBuf::from(checkout_path),
                })
            })
            .collect();

        let agent_response = self.run_json(["agent", "list"])?;
        let agents = array_at(&agent_response, "/result/agents")?
            .iter()
            .map(|agent| {
                Ok(RuntimeAgent {
                    workspace_id: string_at(agent, "/workspace_id")?.to_owned(),
                    pane_id: string_at(agent, "/pane_id")?.to_owned(),
                    name: agent.get("name").and_then(Value::as_str).map(str::to_owned),
                    kind: agent
                        .get("agent")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    status: string_at(agent, "/agent_status")?.to_owned(),
                    session: agent
                        .get("agent_session")
                        .map(parse_agent_session)
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(RuntimeSnapshot { workspaces, agents })
    }

    fn runtime_namespace(&self) -> Option<String> {
        self.runtime_namespace.clone()
    }

    fn ensure_project_workspace(&self, project: &Project) -> Result<String> {
        let workspace_response = self.run_json(["workspace", "list"])?;
        let pane_response = self.run_json(["pane", "list"])?;
        let panes = array_at(&pane_response, "/result/panes")?;

        for workspace in array_at(&workspace_response, "/result/workspaces")? {
            let id = string_at(workspace, "/workspace_id")?;
            let label = workspace.get("label").and_then(Value::as_str);
            let checkout_path = workspace
                .pointer("/worktree/checkout_path")
                .and_then(Value::as_str)
                .or_else(|| {
                    panes.iter().find_map(|pane| {
                        (pane.pointer("/workspace_id").and_then(Value::as_str) == Some(id))
                            .then(|| pane.pointer("/cwd").and_then(Value::as_str))
                            .flatten()
                    })
                });
            if label == Some(project.name.as_str())
                && checkout_path.is_some_and(|path| same_path(Path::new(path), &project.path))
            {
                return Ok(id.to_owned());
            }
        }

        let response = self.run_json([
            OsString::from("workspace"),
            OsString::from("create"),
            OsString::from("--cwd"),
            project.path.as_os_str().to_owned(),
            OsString::from("--label"),
            OsString::from(&project.name),
            OsString::from("--no-focus"),
        ])?;
        Ok(string_at(&response, "/result/workspace/workspace_id")?.to_owned())
    }

    fn create_worktree(
        &self,
        project: &Project,
        source_workspace_id: &str,
        session_name: &str,
    ) -> Result<CreatedWorktree> {
        let plan = detached_worktree_plan(&project.path, &project.base_branch)?;
        let args = vec![
            OsString::from("worktree"),
            OsString::from("create"),
            OsString::from("--workspace"),
            OsString::from(source_workspace_id),
            OsString::from("--branch"),
            OsString::from(&plan.temporary_branch),
            OsString::from("--base"),
            OsString::from(&plan.base_commit),
            OsString::from("--label"),
            OsString::from(session_name),
            OsString::from("--focus"),
            OsString::from("--json"),
        ];
        let output = self.run(args)?;
        let create_response = serde_json::from_slice::<Value>(&output)
            .context("parse Herdr JSON response")
            .and_then(|response| {
                let workspace = opened_workspace(&response)?;
                Ok((workspace, response))
            });
        let (mut workspace, response) = match create_response {
            Ok(created) => created,
            Err(create_error) => {
                let Some(worktree_path) =
                    worktree_path_for_branch(&project.path, &plan.temporary_branch)?
                else {
                    return Err(create_error);
                };
                let recovery = self
                    .run_json([
                        OsString::from("worktree"),
                        OsString::from("open"),
                        OsString::from("--workspace"),
                        OsString::from(source_workspace_id),
                        OsString::from("--path"),
                        worktree_path.as_os_str().to_owned(),
                        OsString::from("--label"),
                        OsString::from(session_name),
                        OsString::from("--focus"),
                        OsString::from("--json"),
                    ])
                    .and_then(|response| {
                        let workspace = opened_workspace(&response)?;
                        Ok((workspace, response))
                    })
                    .with_context(|| {
                        format!(
                            "recover created Herdr worktree after an invalid create response: {create_error:#}"
                        )
                    });
                match recovery {
                    Ok(recovered) => recovered,
                    Err(recovery_error) => {
                        return Ok(CreatedWorktree {
                            workspace: OpenedWorkspace {
                                workspace_id: String::new(),
                                pane_id: String::new(),
                                tab_id: None,
                                worktree_path,
                            },
                            pending_temporary_branch: Some(plan.temporary_branch.clone()),
                            warning: Some(format!(
                                "could not recover its Herdr workspace after creation: {recovery_error:#}"
                            )),
                        });
                    }
                }
            }
        };
        let rename_warning = match self.rename_root_tab(&response, session_name) {
            Ok(tab_id) => {
                workspace.tab_id = Some(tab_id);
                None
            }
            Err(error) => Some(format!("could not rename its root tab: {error:#}")),
        };
        let detach_warning = detach_created_worktree(
            &project.path,
            &workspace.worktree_path,
            &plan.temporary_branch,
        )
        .err()
        .map(|error| {
            format!(
                "could not finish detaching its worktree from temporary branch {:?}: {error:#}",
                plan.temporary_branch
            )
        });
        let pending_temporary_branch = detach_warning.as_ref().map(|_| plan.temporary_branch);
        let warning = match (rename_warning, detach_warning) {
            (Some(rename), Some(detach)) => Some(format!("{rename}; {detach}")),
            (Some(warning), None) | (None, Some(warning)) => Some(warning),
            (None, None) => None,
        };
        Ok(CreatedWorktree {
            workspace,
            pending_temporary_branch,
            warning,
        })
    }

    fn finish_detach(
        &self,
        project: &Project,
        session: &Session,
        temporary_branch: &str,
    ) -> Result<()> {
        detach_created_worktree(&project.path, &session.worktree_path, temporary_branch)
    }

    fn open_worktree(
        &self,
        _project: &Project,
        source_workspace_id: &str,
        session: &Session,
    ) -> Result<OpenedWorkspace> {
        let args = vec![
            OsString::from("worktree"),
            OsString::from("open"),
            OsString::from("--workspace"),
            OsString::from(source_workspace_id),
            OsString::from("--path"),
            session.worktree_path.as_os_str().to_owned(),
            OsString::from("--label"),
            OsString::from(&session.name),
            OsString::from("--focus"),
            OsString::from("--json"),
        ];
        let response = self.run_json(args)?;
        self.opened_workspace_with_tab_label(&response, &session.name)
    }

    fn open_local(&self, project: &Project, session: &Session) -> Result<OpenedWorkspace> {
        let response = self.run_json([
            OsString::from("workspace"),
            OsString::from("create"),
            OsString::from("--cwd"),
            project.path.as_os_str().to_owned(),
            OsString::from("--label"),
            OsString::from(&project.name),
            OsString::from("--focus"),
        ])?;
        let tab_id = self.rename_root_tab(&response, &session.name)?;
        Ok(OpenedWorkspace {
            workspace_id: string_at(&response, "/result/workspace/workspace_id")?.to_owned(),
            pane_id: string_at(&response, "/result/root_pane/pane_id")?.to_owned(),
            tab_id: Some(tab_id),
            worktree_path: project.path.clone(),
        })
    }

    fn focus_workspace(&self, workspace_id: &str) -> Result<()> {
        self.run_json(["workspace", "focus", workspace_id])?;
        Ok(())
    }

    fn focus_agent(&self, pane_id: &str) -> Result<()> {
        self.run_json(["agent", "focus", pane_id])?;
        Ok(())
    }

    fn find_reusable_session_pane(
        &self,
        workspace_id: &str,
        session_name: &str,
        cwd: &Path,
        owned_tab_id: Option<&str>,
    ) -> Result<Option<CreatedAgentPane>> {
        let tab_response = self.run_json(["tab", "list", "--workspace", workspace_id])?;
        let pane_response = self.run_json(["pane", "list", "--workspace", workspace_id])?;
        let panes = array_at(&pane_response, "/result/panes")?;
        let mut best = None;

        for tab in array_at(&tab_response, "/result/tabs")? {
            let tab_id = string_at(tab, "/tab_id")?;
            if tab.get("label").and_then(Value::as_str) != Some(session_name)
                || owned_tab_id.is_some_and(|owned| owned != tab_id)
            {
                continue;
            }
            let tab_panes = panes
                .iter()
                .filter(|pane| pane.get("tab_id").and_then(Value::as_str) == Some(tab_id))
                .collect::<Vec<_>>();
            let [pane] = tab_panes.as_slice() else {
                continue;
            };
            if pane.get("agent").and_then(Value::as_str).is_some()
                || !pane
                    .get("cwd")
                    .and_then(Value::as_str)
                    .is_some_and(|path| same_path(Path::new(path), cwd))
            {
                continue;
            }
            let pane_id = string_at(pane, "/pane_id")?;
            let Ok(process_response) = self.run_json(["pane", "process-info", "--pane", pane_id])
            else {
                continue;
            };
            let foreground_group = process_response
                .pointer("/result/process_info/foreground_process_group_id")
                .and_then(Value::as_i64);
            let shell_pid = process_response
                .pointer("/result/process_info/shell_pid")
                .and_then(Value::as_i64);
            if foreground_group.is_none() || foreground_group != shell_pid {
                continue;
            }
            let candidate = (
                tab.get("focused").and_then(Value::as_bool).unwrap_or(false),
                tab.get("number").and_then(Value::as_i64).unwrap_or(0),
                CreatedAgentPane {
                    tab_id: tab_id.to_owned(),
                    pane_id: pane_id.to_owned(),
                },
            );
            if best
                .as_ref()
                .is_none_or(|current: &(bool, i64, CreatedAgentPane)| {
                    (candidate.0, candidate.1) > (current.0, current.1)
                })
            {
                best = Some(candidate);
            }
        }

        Ok(best.map(|(_, _, pane)| pane))
    }

    fn create_agent_pane(
        &self,
        workspace_id: &str,
        session_name: &str,
        cwd: &Path,
    ) -> Result<CreatedAgentPane> {
        let response = self.run_json([
            OsString::from("tab"),
            OsString::from("create"),
            OsString::from("--workspace"),
            OsString::from(workspace_id),
            OsString::from("--cwd"),
            cwd.as_os_str().to_owned(),
            OsString::from("--label"),
            OsString::from(session_name),
            OsString::from("--focus"),
        ])?;
        Ok(CreatedAgentPane {
            tab_id: string_at(&response, "/result/tab/tab_id")?.to_owned(),
            pane_id: string_at(&response, "/result/root_pane/pane_id")?.to_owned(),
        })
    }

    fn close_tab(&self, tab_id: &str) -> Result<()> {
        self.run_json(["tab", "close", tab_id])?;
        Ok(())
    }

    fn close_agent_tab(&self, pane_id: &str) -> Result<()> {
        let response = self.run_json(["pane", "get", pane_id])?;
        let tab_id = string_at(&response, "/result/pane/tab_id")?;
        self.run_json(["tab", "close", tab_id])?;
        Ok(())
    }

    fn workspace_for_tab(&self, tab_id: &str) -> Result<Option<String>> {
        let response = self.run_json(["tab", "list"])?;
        for tab in array_at(&response, "/result/tabs")? {
            if string_at(tab, "/tab_id")? == tab_id {
                return Ok(Some(string_at(tab, "/workspace_id")?.to_owned()));
            }
        }
        Ok(None)
    }

    fn close_workspace(&self, workspace_id: &str) -> Result<()> {
        self.run_json(["workspace", "close", workspace_id])?;
        Ok(())
    }

    fn start_agent(&self, name: &str, kind: &str, pane_id: &str, args: &[String]) -> Result<()> {
        let mut command = vec![
            OsString::from("agent"),
            OsString::from("start"),
            OsString::from(name),
            OsString::from("--kind"),
            OsString::from(kind),
            OsString::from("--pane"),
            OsString::from(pane_id),
        ];
        if !args.is_empty() {
            command.push(OsString::from("--"));
            command.extend(args.iter().map(OsString::from));
        }
        self.run_json(command)?;
        Ok(())
    }
}

impl CliHerdr {
    fn opened_workspace_with_tab_label(
        &self,
        response: &Value,
        label: &str,
    ) -> Result<OpenedWorkspace> {
        let mut workspace = opened_workspace(response)?;
        workspace.tab_id = Some(self.rename_root_tab(response, label)?);
        Ok(workspace)
    }

    fn rename_root_tab(&self, response: &Value, label: &str) -> Result<String> {
        let direct_tab_id = response
            .pointer("/result/tab/tab_id")
            .or_else(|| response.pointer("/result/root_pane/tab_id"))
            .and_then(Value::as_str);
        let pane_response;
        let tab_id = if let Some(tab_id) = direct_tab_id {
            tab_id
        } else {
            let pane_id = string_at(response, "/result/root_pane/pane_id")?;
            pane_response = self.run_json(["pane", "get", pane_id])?;
            string_at(&pane_response, "/result/pane/tab_id")?
        };
        self.run_json([
            OsString::from("tab"),
            OsString::from("rename"),
            OsString::from(tab_id),
            OsString::from(label),
        ])?;
        Ok(tab_id.to_owned())
    }
}

fn opened_workspace(response: &Value) -> Result<OpenedWorkspace> {
    Ok(OpenedWorkspace {
        workspace_id: string_at(response, "/result/workspace/workspace_id")?.to_owned(),
        pane_id: string_at(response, "/result/root_pane/pane_id")?.to_owned(),
        tab_id: response
            .pointer("/result/tab/tab_id")
            .or_else(|| response.pointer("/result/root_pane/tab_id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        worktree_path: PathBuf::from(string_at(response, "/result/worktree/path")?),
    })
}

fn parse_agent_session(value: &Value) -> Result<AgentSession> {
    Ok(AgentSession {
        agent: string_at(value, "/agent")?.to_owned(),
        kind: string_at(value, "/kind")?.to_owned(),
        value: string_at(value, "/value")?.to_owned(),
    })
}

fn array_at<'a>(value: &'a Value, pointer: &str) -> Result<&'a Vec<Value>> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .with_context(|| format!("Herdr response is missing array {pointer}"))
}

fn string_at<'a>(value: &'a Value, pointer: &str) -> Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .with_context(|| format!("Herdr response is missing string {pointer}"))
}
