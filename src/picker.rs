use std::{
    fs,
    io::{self, Write, stdout},
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use crossterm::{
    cursor::Show,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEvent, KeyEventKind,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Flex, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row, Table,
        TableState,
    },
};

use crate::{
    coordinator::{
        Herdr, activate_existing, agent_name, create_session, delete_session, sync_agent_sessions,
    },
    herdr::CliHerdr,
    model::{Config, DEFAULT_BASE_BRANCH, Project, RuntimeSnapshot, Session, SessionMode, State},
    paths::same_path,
    repository::{normalize_project, repair_base_branch},
    store::Store,
    theme::Theme,
};

const ROW_HEIGHT: u16 = 2;
const DIRECTORY_ROW_HEIGHT: u16 = 1;
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);
const DEFAULT_AGENT: &str = "pi";
const BASE_BRANCH_LABEL: &str = "auto branch";
const DEFAULT_SESSION_MODE: &str = "worktree";

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClickTarget {
    Project(String),
    Session {
        project_id: String,
        session_name: String,
    },
    Directory(usize),
}

#[derive(Debug, Clone, Copy)]
struct AddProjectLayout {
    popup: Rect,
    path: Rect,
    defaults: Rect,
    hidden_toggle: Rect,
    directory_rows: Rect,
    add_current: Rect,
}

#[derive(Debug, Clone, Copy)]
struct NewSessionLayout {
    popup: Rect,
    description: Rect,
    mode_worktree: Rect,
    mode_local: Rect,
    title: Rect,
    create: Rect,
    cancel: Rect,
}

#[derive(Debug, Clone, Copy)]
struct DeleteConfirmationLayout {
    popup: Rect,
    body: Rect,
    delete: Rect,
    cancel: Rect,
}

#[derive(Debug, Clone, Copy)]
struct ContextMenuLayout {
    popup: Rect,
    pin: Rect,
    remove: Rect,
}

impl ContextMenuLayout {
    fn new(area: Rect, column: u16, row: u16) -> Self {
        let width = 20.min(area.width);
        let height = 4.min(area.height);
        let x = column.min(area.right().saturating_sub(width));
        let y = row.min(area.bottom().saturating_sub(height));
        let popup = Rect::new(x, y, width, height);
        let inner = Block::bordered().inner(popup);
        let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);
        Self {
            popup,
            pin: rows[0],
            remove: rows[1],
        }
    }
}

impl DeleteConfirmationLayout {
    fn new(area: Rect) -> Self {
        let width = area.width.saturating_sub(4).clamp(1, 64);
        let height = area.height.saturating_sub(4).clamp(1, 11);
        let popup = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        let inner = Block::bordered().inner(popup);
        let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(inner);
        let actions = Layout::horizontal([
            Constraint::Length(14),
            Constraint::Length(1),
            Constraint::Length(14),
            Constraint::Min(0),
        ])
        .split(rows[1]);
        Self {
            popup,
            body: rows[0],
            delete: actions[0],
            cancel: actions[2],
        }
    }
}

impl NewSessionLayout {
    fn new(area: Rect) -> Self {
        let width = area.width.saturating_sub(4).clamp(1, 64);
        let height = area.height.saturating_sub(4).clamp(1, 13);
        let [centered_row] = Layout::vertical([Constraint::Length(height)])
            .flex(Flex::Center)
            .areas(area);
        let [popup] = Layout::horizontal([Constraint::Length(width)])
            .flex(Flex::Center)
            .areas(centered_row);
        let inner = Block::bordered().inner(popup);
        let rows = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(inner);
        let modes = Layout::horizontal([
            Constraint::Length(7),
            Constraint::Length(13),
            Constraint::Length(9),
        ])
        .flex(Flex::Center)
        .split(rows[1]);
        let actions = Layout::horizontal([
            Constraint::Length(14),
            Constraint::Length(1),
            Constraint::Length(14),
        ])
        .flex(Flex::Center)
        .split(rows[3]);
        Self {
            popup,
            description: rows[0],
            mode_worktree: modes[1],
            mode_local: modes[2],
            title: rows[2],
            create: actions[0],
            cancel: actions[2],
        }
    }
}

impl AddProjectLayout {
    fn new(area: Rect) -> Self {
        let width = area.width.saturating_sub(4).clamp(1, 72);
        let height = area.height.saturating_sub(4).clamp(1, 22);
        let popup = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        let inner = Block::bordered().inner(popup);
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(1),
        ])
        .split(inner);
        let defaults =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(16)]).split(chunks[1]);
        let directory_rows = chunks[2];
        let add_current = Rect::new(
            directory_rows.x,
            directory_rows.y,
            directory_rows.width,
            DIRECTORY_ROW_HEIGHT.min(directory_rows.height),
        );
        Self {
            popup,
            path: chunks[0],
            defaults: defaults[0],
            hidden_toggle: defaults[1],
            directory_rows,
            add_current,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct UiLayout {
    projects_panel: Rect,
    project_search: Rect,
    add_project: Rect,
    project_rows: Rect,
    sessions_panel: Rect,
    session_search: Rect,
    new_session: Rect,
    session_rows: Rect,
}

impl UiLayout {
    fn new(area: Rect) -> Self {
        let content = area.inner(Margin::new(2, 1));
        let panels = Layout::horizontal([
            Constraint::Percentage(32),
            Constraint::Length(2),
            Constraint::Min(1),
        ])
        .split(content);
        let projects_panel = panels[0];
        let sessions_panel = panels[2];
        let projects_inner = Block::bordered().inner(projects_panel);
        let project_chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(projects_inner);

        let sessions_inner = Block::bordered().inner(sessions_panel);
        let session_chunks =
            Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(sessions_inner);
        let session_actions =
            Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(session_chunks[0]);
        Self {
            projects_panel,
            project_search: project_chunks[0],
            add_project: project_chunks[1],
            project_rows: project_chunks[2],
            sessions_panel,
            session_search: session_actions[0],
            new_session: session_actions[1],
            session_rows: session_chunks[1],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    None,
    Quit,
    ActivateSession {
        project_id: String,
        session_name: String,
    },
    CreateSession {
        project_id: String,
        session_name: String,
        mode: SessionMode,
    },
    AddProject(Project),
    DeleteProject {
        project_id: String,
    },
    DeleteSession {
        project_id: String,
        session_name: String,
    },
    ToggleProjectPin {
        project_id: String,
    },
    ToggleSessionPin {
        project_id: String,
        session_name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeleteTarget {
    Project {
        project_id: String,
        project_name: String,
    },
    Session {
        project_id: String,
        session_name: String,
        mode: SessionMode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum View {
    Browser,
    AddProject(ProjectDraft),
    NewSession {
        project_id: String,
        input: String,
        mode: SessionMode,
    },
    ContextMenu {
        target: DeleteTarget,
        column: u16,
        row: u16,
        selected: usize,
    },
    ConfirmDelete(DeleteTarget),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusedPane {
    Projects,
    Sessions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectDraft {
    current_dir: PathBuf,
    directories: Vec<PathBuf>,
    show_hidden: bool,
    filter: String,
    selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DirectoryChoice {
    Current,
    Navigate(PathBuf),
}

impl ProjectDraft {
    fn open_with_hidden(path: PathBuf, show_hidden: bool) -> Result<Self> {
        let current_dir = fs::canonicalize(&path)
            .with_context(|| format!("open directory {}", path.display()))?;
        let mut directories = fs::read_dir(&current_dir)
            .with_context(|| format!("read directory {}", current_dir.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        directories.sort_by_cached_key(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase()
        });
        Ok(Self {
            current_dir,
            directories,
            show_hidden,
            filter: String::new(),
            selected: 0,
        })
    }

    fn row_count(&self) -> usize {
        1 + usize::from(self.current_dir.parent().is_some()) + self.visible_directories().count()
    }

    fn visible_directories(&self) -> impl Iterator<Item = &PathBuf> {
        let filter = self.filter.to_lowercase();
        self.directories.iter().filter(move |path| {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            (self.show_hidden || !name.starts_with('.'))
                && (filter.is_empty() || name.contains(&filter))
        })
    }

    fn select_first_filter_match(&mut self) {
        self.selected = if self.filter.is_empty() || self.visible_directories().next().is_none() {
            0
        } else {
            1 + usize::from(self.current_dir.parent().is_some())
        };
    }

    fn choice(&self, position: usize) -> Option<DirectoryChoice> {
        if position == 0 {
            return Some(DirectoryChoice::Current);
        }
        let mut position = position - 1;
        if let Some(parent) = self.current_dir.parent() {
            if position == 0 {
                return Some(DirectoryChoice::Navigate(parent.to_path_buf()));
            }
            position -= 1;
        }
        self.visible_directories()
            .nth(position)
            .cloned()
            .map(DirectoryChoice::Navigate)
    }
}

pub struct Picker {
    pub config: Config,
    pub state: State,
    pub snapshot: RuntimeSnapshot,
    view: View,
    focused_pane: FocusedPane,
    project_selected: usize,
    session_selected: usize,
    project_offset: usize,
    session_offset: usize,
    directory_offset: usize,
    project_filter: String,
    session_filter: String,
    searching: bool,
    theme: Theme,
    pub error: Option<String>,
    last_click: Option<(ClickTarget, Instant)>,
}

impl Picker {
    pub fn new(config: Config, state: State, snapshot: RuntimeSnapshot) -> Self {
        let project_selected = usize::from(!config.projects.is_empty());
        let theme = Theme::from(config.ui.theme);
        Self {
            config,
            state,
            snapshot,
            view: View::Browser,
            focused_pane: FocusedPane::Projects,
            project_selected,
            session_selected: 0,
            project_offset: 0,
            session_offset: 0,
            directory_offset: 0,
            project_filter: String::new(),
            session_filter: String::new(),
            searching: false,
            theme,
            error: None,
            last_click: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Intent {
        if self.error.is_some() {
            self.error = None;
            return Intent::None;
        }
        if self.searching {
            let previous_project = (self.focused_pane == FocusedPane::Projects)
                .then(|| self.active_project_id())
                .flatten();
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => match self.focused_pane {
                    FocusedPane::Projects => {
                        self.project_filter.pop();
                    }
                    FocusedPane::Sessions => {
                        self.session_filter.pop();
                    }
                },
                KeyCode::Char(character) => match self.focused_pane {
                    FocusedPane::Projects => self.project_filter.push(character),
                    FocusedPane::Sessions => self.session_filter.push(character),
                },
                _ => {}
            }
            let max_selected = self.row_count().saturating_sub(1);
            match self.focused_pane {
                FocusedPane::Projects => {
                    self.project_selected = self.project_selected.min(max_selected);
                }
                FocusedPane::Sessions => {
                    self.session_selected = self.session_selected.min(max_selected);
                }
            }
            if self.focused_pane == FocusedPane::Projects
                && previous_project != self.active_project_id()
            {
                self.reset_session_cursor();
            }
            self.last_click = None;
            return Intent::None;
        }

        self.last_click = None;
        match self.view.clone() {
            View::Browser => match self.focused_pane {
                FocusedPane::Projects => self.handle_projects(key.code),
                FocusedPane::Sessions => self.handle_sessions(key.code),
            },
            View::NewSession {
                project_id,
                input,
                mode,
            } => self.handle_new_session(key.code, project_id, input, mode),
            View::AddProject(draft) => self.handle_add_project(key.code, draft),
            View::ContextMenu {
                target,
                column,
                row,
                selected,
            } => self.handle_context_menu(key.code, target, column, row, selected),
            View::ConfirmDelete(target) => self.handle_delete_confirmation(key.code, target),
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> Intent {
        self.handle_mouse_at(mouse, area, Instant::now())
    }

    fn handle_mouse_at(&mut self, mouse: MouseEvent, area: Rect, now: Instant) -> Intent {
        if self.error.is_some() {
            self.error = None;
            return Intent::None;
        }
        if matches!(self.view, View::AddProject(_)) {
            return self.handle_add_project_mouse(mouse, area, now);
        }
        if let View::NewSession {
            project_id,
            input,
            mode,
        } = self.view.clone()
        {
            return self.handle_new_session_mouse(mouse, area, project_id, input, mode);
        }
        if let View::ConfirmDelete(target) = self.view.clone() {
            return self.handle_delete_confirmation_mouse(mouse, area, target);
        }
        if let View::ContextMenu {
            target,
            column,
            row,
            selected,
        } = self.view.clone()
        {
            return self.handle_context_menu_mouse(mouse, area, target, column, row, selected);
        }

        let layout = UiLayout::new(area);
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.handle_scroll(mouse, layout, false);
                Intent::None
            }
            MouseEventKind::ScrollDown => {
                self.handle_scroll(mouse, layout, true);
                Intent::None
            }
            MouseEventKind::Down(MouseButton::Left) => self.handle_left_click(mouse, layout, now),
            MouseEventKind::Down(MouseButton::Right) => self.handle_right_click(mouse, layout),
            _ => Intent::None,
        }
    }

    fn handle_delete_confirmation(&mut self, key: KeyCode, target: DeleteTarget) -> Intent {
        match key {
            KeyCode::Esc | KeyCode::Char('n') => {
                self.view = View::Browser;
                Intent::None
            }
            KeyCode::Enter | KeyCode::Char('y') => Self::delete_intent(target),
            _ => Intent::None,
        }
    }

    fn handle_context_menu(
        &mut self,
        key: KeyCode,
        target: DeleteTarget,
        column: u16,
        row: u16,
        mut selected: usize,
    ) -> Intent {
        match key {
            KeyCode::Esc => {
                self.view = View::Browser;
                Intent::None
            }
            KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') => {
                selected = 1 - selected;
                self.view = View::ContextMenu {
                    target,
                    column,
                    row,
                    selected,
                };
                Intent::None
            }
            KeyCode::Enter if selected == 0 => {
                self.view = View::Browser;
                Self::pin_intent(target)
            }
            KeyCode::Enter => {
                self.open_delete_confirmation(target);
                Intent::None
            }
            _ => Intent::None,
        }
    }

    fn handle_context_menu_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        target: DeleteTarget,
        column: u16,
        row: u16,
        selected: usize,
    ) -> Intent {
        let layout = ContextMenuLayout::new(area, column, row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if contains(layout.pin, mouse) => {
                self.view = View::Browser;
                Self::pin_intent(target)
            }
            MouseEventKind::Down(MouseButton::Left) if contains(layout.remove, mouse) => {
                self.open_delete_confirmation(target);
                Intent::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.view = View::Browser;
                Intent::None
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.view = View::ContextMenu {
                    target,
                    column,
                    row,
                    selected: 1 - selected,
                };
                Intent::None
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.view = View::Browser;
                self.handle_right_click(mouse, UiLayout::new(area))
            }
            _ => Intent::None,
        }
    }

    fn pin_intent(target: DeleteTarget) -> Intent {
        match target {
            DeleteTarget::Project { project_id, .. } => Intent::ToggleProjectPin { project_id },
            DeleteTarget::Session {
                project_id,
                session_name,
                ..
            } => Intent::ToggleSessionPin {
                project_id,
                session_name,
            },
        }
    }

    fn open_delete_confirmation(&mut self, target: DeleteTarget) {
        if let DeleteTarget::Project { project_id, .. } = &target
            && self
                .state
                .sessions
                .iter()
                .any(|session| &session.project_id == project_id)
        {
            self.view = View::Browser;
            self.error = Some("Delete its sessions first, then delete the project.".into());
            return;
        }
        self.view = View::ConfirmDelete(target);
    }

    fn handle_delete_confirmation_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        target: DeleteTarget,
    ) -> Intent {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Intent::None;
        }
        let layout = DeleteConfirmationLayout::new(area);
        if contains(layout.cancel, mouse) {
            self.view = View::Browser;
            return Intent::None;
        }
        if contains(layout.delete, mouse) {
            return Self::delete_intent(target);
        }
        Intent::None
    }

    fn delete_intent(target: DeleteTarget) -> Intent {
        match target {
            DeleteTarget::Project { project_id, .. } => Intent::DeleteProject { project_id },
            DeleteTarget::Session {
                project_id,
                session_name,
                ..
            } => Intent::DeleteSession {
                project_id,
                session_name,
            },
        }
    }

    fn handle_new_session_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        project_id: String,
        input: String,
        mut mode: SessionMode,
    ) -> Intent {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Intent::None;
        }
        let layout = NewSessionLayout::new(area);
        if contains(layout.mode_worktree, mouse) {
            mode = SessionMode::Worktree;
            self.view = View::NewSession {
                project_id,
                input,
                mode,
            };
            return Intent::None;
        }
        if contains(layout.mode_local, mouse) {
            mode = SessionMode::Local;
            self.view = View::NewSession {
                project_id,
                input,
                mode,
            };
            return Intent::None;
        }
        if contains(layout.cancel, mouse) {
            self.view = View::Browser;
            self.focused_pane = FocusedPane::Sessions;
            return Intent::None;
        }
        if contains(layout.create, mouse) && !input.trim().is_empty() {
            return Intent::CreateSession {
                project_id,
                session_name: input.trim().to_owned(),
                mode,
            };
        }
        Intent::None
    }

    fn handle_add_project_mouse(&mut self, mouse: MouseEvent, area: Rect, now: Instant) -> Intent {
        let View::AddProject(mut draft) = self.view.clone() else {
            return Intent::None;
        };
        let layout = AddProjectLayout::new(area);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if contains(layout.hidden_toggle, mouse) => {
                draft.show_hidden = !draft.show_hidden;
                draft.select_first_filter_match();
                self.directory_offset = 0;
                self.last_click = None;
                self.view = View::AddProject(draft);
                Intent::None
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if contains(layout.directory_rows, mouse) =>
            {
                let count = draft.row_count();
                if count > 0 {
                    draft.selected = if mouse.kind == MouseEventKind::ScrollDown {
                        (draft.selected + 1) % count
                    } else if draft.selected == 0 {
                        count - 1
                    } else {
                        draft.selected - 1
                    };
                }
                self.last_click = None;
                self.view = View::AddProject(draft);
                Intent::None
            }
            MouseEventKind::Down(MouseButton::Left)
                if self.directory_offset == 0 && contains(layout.add_current, mouse) =>
            {
                self.last_click = None;
                self.project_from_directory(&draft.current_dir)
            }
            MouseEventKind::Down(MouseButton::Left) if contains(layout.directory_rows, mouse) => {
                let position = self.directory_offset
                    + usize::from((mouse.row - layout.directory_rows.y) / DIRECTORY_ROW_HEIGHT);
                if draft.choice(position).is_none() {
                    self.last_click = None;
                    return Intent::None;
                }
                draft.selected = position;
                self.view = View::AddProject(draft.clone());
                if self.register_click(ClickTarget::Directory(position), now) {
                    self.activate_directory_choice(draft, position)
                } else {
                    Intent::None
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.last_click = None;
                Intent::None
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.last_click = None;
                Intent::None
            }
            _ => Intent::None,
        }
    }

    fn handle_scroll(&mut self, mouse: MouseEvent, layout: UiLayout, down: bool) {
        self.searching = false;
        self.last_click = None;
        if contains(layout.projects_panel, mouse) {
            self.focus_projects();
        } else if contains(layout.sessions_panel, mouse) {
            if self.active_project_id().is_none() {
                return;
            }
            self.focused_pane = FocusedPane::Sessions;
        } else {
            return;
        }
        if down {
            self.select_next();
        } else {
            self.select_previous();
        }
    }

    fn handle_left_click(&mut self, mouse: MouseEvent, layout: UiLayout, now: Instant) -> Intent {
        if contains(layout.project_search, mouse) {
            self.focus_projects();
            self.searching = true;
            self.last_click = None;
            return Intent::None;
        }
        if contains(layout.add_project, mouse) {
            self.searching = false;
            self.last_click = None;
            self.open_add_project();
            return Intent::None;
        }
        if contains(layout.project_rows, mouse) {
            let Some(position) = table_row_at(layout.project_rows, mouse, self.project_offset)
            else {
                self.last_click = None;
                return Intent::None;
            };
            let projects = self.filtered_project_indices();
            let Some(index) = projects.get(position).copied() else {
                self.last_click = None;
                return Intent::None;
            };
            let project_id = self.config.projects[index].id.clone();
            let project_changed = self.active_project_id().as_deref() != Some(&project_id);
            self.focused_pane = FocusedPane::Projects;
            self.project_selected = position + 1;
            if project_changed {
                self.reset_session_cursor();
            }
            self.searching = false;
            if self.register_click(ClickTarget::Project(project_id.clone()), now) {
                self.focused_pane = FocusedPane::Sessions;
                self.session_selected = 0;
            }
            return Intent::None;
        }

        let Some(project_id) = self.active_project_id() else {
            return Intent::None;
        };
        if contains(layout.session_search, mouse) {
            self.focused_pane = FocusedPane::Sessions;
            self.searching = true;
            self.last_click = None;
            return Intent::None;
        }
        if contains(layout.new_session, mouse) {
            self.searching = false;
            self.last_click = None;
            self.open_new_session(&project_id);
            return Intent::None;
        }
        if contains(layout.session_rows, mouse) {
            let Some(position) = table_row_at(layout.session_rows, mouse, self.session_offset)
            else {
                self.last_click = None;
                return Intent::None;
            };
            let sessions = self.filtered_session_indices(&project_id);
            let Some(index) = sessions.get(position).copied() else {
                self.last_click = None;
                return Intent::None;
            };
            let session_name = self.state.sessions[index].name.clone();
            self.focused_pane = FocusedPane::Sessions;
            self.session_selected = position + 1;
            self.searching = false;
            let target = ClickTarget::Session {
                project_id: project_id.clone(),
                session_name: session_name.clone(),
            };
            return if self.register_click(target, now) {
                Intent::ActivateSession {
                    project_id,
                    session_name,
                }
            } else {
                Intent::None
            };
        }
        self.last_click = None;
        Intent::None
    }

    fn handle_right_click(&mut self, mouse: MouseEvent, layout: UiLayout) -> Intent {
        self.searching = false;
        self.last_click = None;
        if contains(layout.project_rows, mouse) {
            let Some(position) = table_row_at(layout.project_rows, mouse, self.project_offset)
            else {
                return Intent::None;
            };
            let projects = self.filtered_project_indices();
            let Some(index) = projects.get(position).copied() else {
                return Intent::None;
            };
            let project_id = self.config.projects[index].id.clone();
            let project_name = self.config.projects[index].name.clone();
            let project_changed = self.active_project_id().as_deref() != Some(&project_id);
            self.focused_pane = FocusedPane::Projects;
            self.project_selected = position + 1;
            if project_changed {
                self.reset_session_cursor();
            }
            self.view = View::ContextMenu {
                target: DeleteTarget::Project {
                    project_id,
                    project_name,
                },
                column: mouse.column,
                row: mouse.row,
                selected: 0,
            };
            return Intent::None;
        }

        let Some(project_id) = self.active_project_id() else {
            return Intent::None;
        };
        if contains(layout.session_rows, mouse) {
            let Some(position) = table_row_at(layout.session_rows, mouse, self.session_offset)
            else {
                return Intent::None;
            };
            let sessions = self.filtered_session_indices(&project_id);
            let Some(index) = sessions.get(position).copied() else {
                return Intent::None;
            };
            let session = &self.state.sessions[index];
            self.focused_pane = FocusedPane::Sessions;
            self.session_selected = position + 1;
            self.view = View::ContextMenu {
                target: DeleteTarget::Session {
                    project_id,
                    session_name: session.name.clone(),
                    mode: session.mode,
                },
                column: mouse.column,
                row: mouse.row,
                selected: 0,
            };
        }
        Intent::None
    }

    fn register_click(&mut self, target: ClickTarget, now: Instant) -> bool {
        let double_click = self.last_click.as_ref().is_some_and(|(previous, at)| {
            previous == &target
                && now
                    .checked_duration_since(*at)
                    .is_some_and(|elapsed| elapsed <= DOUBLE_CLICK_WINDOW)
        });
        self.last_click = if double_click {
            None
        } else {
            Some((target, now))
        };
        double_click
    }

    fn focus_projects(&mut self) {
        self.focused_pane = FocusedPane::Projects;
    }

    fn reset_session_cursor(&mut self) {
        self.session_selected = 0;
        self.session_offset = 0;
    }

    fn active_project_id(&self) -> Option<String> {
        match &self.view {
            View::NewSession { project_id, .. } => Some(project_id.clone()),
            View::ConfirmDelete(DeleteTarget::Project { project_id, .. })
            | View::ConfirmDelete(DeleteTarget::Session { project_id, .. }) => {
                Some(project_id.clone())
            }
            View::ContextMenu {
                target: DeleteTarget::Project { project_id, .. },
                ..
            }
            | View::ContextMenu {
                target: DeleteTarget::Session { project_id, .. },
                ..
            } => Some(project_id.clone()),
            View::Browser | View::AddProject(_) => self
                .project_selected
                .checked_sub(1)
                .and_then(|position| self.filtered_project_indices().get(position).copied())
                .map(|index| self.config.projects[index].id.clone()),
        }
    }

    #[cfg(test)]
    fn displayed_session_name(&self, project_id: &str) -> Option<String> {
        let sessions = self.filtered_session_indices(project_id);
        let position = self.displayed_session_position(!sessions.is_empty())?;
        sessions
            .get(position)
            .map(|index| self.state.sessions[*index].name.clone())
    }

    fn displayed_session_position(&self, has_sessions: bool) -> Option<usize> {
        match (&self.view, self.focused_pane) {
            (View::Browser | View::ContextMenu { .. }, FocusedPane::Sessions) => {
                self.session_selected.checked_sub(1)
            }
            (View::Browser | View::ContextMenu { .. }, FocusedPane::Projects) => Some(0),
            (View::AddProject(_) | View::NewSession { .. } | View::ConfirmDelete(_), _) => None,
        }
        .filter(|_| has_sessions)
    }

    fn project_added(&mut self, config: Config, project_id: &str) {
        self.config = config;
        self.view = View::Browser;
        self.focused_pane = FocusedPane::Projects;
        self.project_filter.clear();
        self.project_selected = self
            .filtered_project_indices()
            .iter()
            .position(|index| self.config.projects[*index].id == project_id)
            .map(|position| position + 1)
            .unwrap_or(0);
        self.reset_session_cursor();
    }

    fn project_deleted(&mut self, project_id: &str) {
        self.config
            .projects
            .retain(|project| project.id != project_id);
        self.view = View::Browser;
        self.focused_pane = FocusedPane::Projects;
        self.project_selected = self
            .project_selected
            .min(self.filtered_project_indices().len());
        self.reset_session_cursor();
    }

    fn session_deleted(&mut self) {
        self.view = View::Browser;
        self.focused_pane = FocusedPane::Sessions;
        let session_count = self
            .active_project_id()
            .map(|project_id| self.filtered_session_indices(&project_id).len())
            .unwrap_or(0);
        self.session_selected = self.session_selected.min(session_count);
        self.session_offset = self.session_offset.min(self.session_selected);
    }

    fn project_pin_toggled(&mut self, config: Config, project_id: &str) {
        self.config = config;
        self.view = View::Browser;
        self.focused_pane = FocusedPane::Projects;
        self.project_selected = self
            .filtered_project_indices()
            .iter()
            .position(|index| self.config.projects[*index].id == project_id)
            .map(|position| position + 1)
            .unwrap_or(0);
        self.reset_session_cursor();
    }

    fn session_pin_toggled(&mut self, config: Config, project_id: &str, session_name: &str) {
        self.config = config;
        self.view = View::Browser;
        self.focused_pane = FocusedPane::Sessions;
        self.session_selected = self
            .filtered_session_indices(project_id)
            .iter()
            .position(|index| self.state.sessions[*index].name == session_name)
            .map(|position| position + 1)
            .unwrap_or(0);
        self.session_offset = 0;
    }

    fn handle_projects(&mut self, key: KeyCode) -> Intent {
        match key {
            KeyCode::Char('q') | KeyCode::Esc => Intent::Quit,
            KeyCode::Char('a') => {
                self.open_add_project();
                Intent::None
            }
            KeyCode::Char('/') => {
                self.searching = true;
                Intent::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_previous();
                Intent::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                Intent::None
            }
            KeyCode::Delete | KeyCode::Backspace => {
                self.confirm_selected_project_delete();
                Intent::None
            }
            KeyCode::Enter if self.project_selected == 0 => {
                self.open_add_project();
                Intent::None
            }
            KeyCode::Enter => {
                let projects = self.filtered_project_indices();
                if projects.get(self.project_selected - 1).is_some() {
                    self.focused_pane = FocusedPane::Sessions;
                    self.session_selected = 0;
                }
                Intent::None
            }
            _ => Intent::None,
        }
    }

    fn handle_sessions(&mut self, key: KeyCode) -> Intent {
        let Some(project_id) = self.active_project_id() else {
            self.focused_pane = FocusedPane::Projects;
            return Intent::None;
        };
        match key {
            KeyCode::Char('q') => Intent::Quit,
            KeyCode::Esc => {
                self.focused_pane = FocusedPane::Projects;
                Intent::None
            }
            KeyCode::Char('n') => {
                self.open_new_session(&project_id);
                Intent::None
            }
            KeyCode::Char('/') => {
                self.searching = true;
                Intent::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_previous();
                Intent::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                Intent::None
            }
            KeyCode::Delete | KeyCode::Backspace => {
                self.confirm_selected_session_delete(&project_id);
                Intent::None
            }
            KeyCode::Enter if self.session_selected == 0 => {
                self.open_new_session(&project_id);
                Intent::None
            }
            KeyCode::Enter => {
                let sessions = self.filtered_session_indices(&project_id);
                sessions
                    .get(self.session_selected - 1)
                    .map(|index| Intent::ActivateSession {
                        project_id,
                        session_name: self.state.sessions[*index].name.clone(),
                    })
                    .unwrap_or(Intent::None)
            }
            _ => Intent::None,
        }
    }

    fn handle_new_session(
        &mut self,
        key: KeyCode,
        project_id: String,
        mut input: String,
        mut mode: SessionMode,
    ) -> Intent {
        match key {
            KeyCode::Esc => {
                self.view = View::Browser;
                self.focused_pane = FocusedPane::Sessions;
                Intent::None
            }
            KeyCode::Backspace => {
                input.pop();
                self.view = View::NewSession {
                    project_id,
                    input,
                    mode,
                };
                Intent::None
            }
            KeyCode::Left | KeyCode::Right => {
                mode = match mode {
                    SessionMode::Local => SessionMode::Worktree,
                    SessionMode::Worktree => SessionMode::Local,
                };
                self.view = View::NewSession {
                    project_id,
                    input,
                    mode,
                };
                Intent::None
            }
            KeyCode::Char(character) => {
                input.push(character);
                self.view = View::NewSession {
                    project_id,
                    input,
                    mode,
                };
                Intent::None
            }
            KeyCode::Enter if !input.trim().is_empty() => Intent::CreateSession {
                project_id,
                session_name: input.trim().to_owned(),
                mode,
            },
            _ => Intent::None,
        }
    }

    fn handle_add_project(&mut self, key: KeyCode, mut draft: ProjectDraft) -> Intent {
        let count = draft.row_count();
        match key {
            KeyCode::Esc if draft.filter.is_empty() => {
                self.view = View::Browser;
                self.focused_pane = FocusedPane::Projects;
                Intent::None
            }
            KeyCode::Esc => {
                draft.filter.clear();
                draft.select_first_filter_match();
                self.directory_offset = 0;
                self.view = View::AddProject(draft);
                Intent::None
            }
            KeyCode::Up => {
                draft.selected = if draft.selected == 0 {
                    count.saturating_sub(1)
                } else {
                    draft.selected - 1
                };
                self.view = View::AddProject(draft);
                Intent::None
            }
            KeyCode::Down => {
                if count > 0 {
                    draft.selected = (draft.selected + 1) % count;
                }
                self.view = View::AddProject(draft);
                Intent::None
            }
            KeyCode::Backspace if draft.filter.is_empty() => {
                if let Some(parent) = draft.current_dir.parent().map(PathBuf::from) {
                    self.open_add_project_at_with_hidden(parent, draft.show_hidden);
                }
                Intent::None
            }
            KeyCode::Backspace => {
                draft.filter.pop();
                draft.select_first_filter_match();
                self.directory_offset = 0;
                self.view = View::AddProject(draft);
                Intent::None
            }
            KeyCode::Char('.') if draft.filter.is_empty() => {
                draft.show_hidden = !draft.show_hidden;
                draft.select_first_filter_match();
                self.directory_offset = 0;
                self.view = View::AddProject(draft);
                Intent::None
            }
            KeyCode::Char(character) => {
                draft.filter.push(character);
                draft.select_first_filter_match();
                self.directory_offset = 0;
                self.view = View::AddProject(draft);
                Intent::None
            }
            KeyCode::Enter
                if draft.selected == 0
                    && !draft.filter.is_empty()
                    && draft.visible_directories().next().is_none() =>
            {
                Intent::None
            }
            KeyCode::Enter => self.activate_directory_choice(draft.clone(), draft.selected),
            _ => Intent::None,
        }
    }

    fn open_add_project(&mut self) {
        let initial_dir = dirs::home_dir()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        self.open_add_project_at(initial_dir);
    }

    fn open_add_project_at(&mut self, path: PathBuf) {
        self.open_add_project_at_with_hidden(path, false);
    }

    fn open_add_project_at_with_hidden(&mut self, path: PathBuf, show_hidden: bool) {
        match ProjectDraft::open_with_hidden(path, show_hidden) {
            Ok(draft) => {
                self.directory_offset = 0;
                self.last_click = None;
                self.view = View::AddProject(draft);
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }

    fn activate_directory_choice(&mut self, draft: ProjectDraft, position: usize) -> Intent {
        match draft.choice(position) {
            Some(DirectoryChoice::Current) => self.project_from_directory(&draft.current_dir),
            Some(DirectoryChoice::Navigate(path)) => {
                self.open_add_project_at_with_hidden(path, draft.show_hidden);
                Intent::None
            }
            None => Intent::None,
        }
    }

    fn project_from_directory(&mut self, path: &std::path::Path) -> Intent {
        if path.to_str().is_none() {
            self.error = Some("Project folder path must be valid UTF-8".into());
            return Intent::None;
        }
        let Some(name) = path.file_name().filter(|name| !name.is_empty()) else {
            self.error = Some("The filesystem root cannot be added as a project".into());
            return Intent::None;
        };
        let name = name.to_string_lossy().into_owned();
        let base_id = match slug(&name) {
            id if id.is_empty() => "project".to_owned(),
            id => id,
        };
        let mut id = base_id.clone();
        let mut suffix = 2;
        while self.config.projects.iter().any(|project| project.id == id) {
            id = format!("{base_id}-{suffix}");
            suffix += 1;
        }
        Intent::AddProject(Project {
            id,
            name,
            path: path.to_path_buf(),
            agent: DEFAULT_AGENT.into(),
            base_branch: DEFAULT_BASE_BRANCH.into(),
            agent_args: Vec::new(),
        })
    }

    fn open_new_session(&mut self, project_id: &str) {
        self.view = View::NewSession {
            project_id: project_id.to_owned(),
            input: String::new(),
            mode: SessionMode::Worktree,
        };
    }

    fn confirm_selected_project_delete(&mut self) {
        let Some(position) = self.project_selected.checked_sub(1) else {
            return;
        };
        let projects = self.filtered_project_indices();
        let Some(index) = projects.get(position).copied() else {
            return;
        };
        let project = self.config.projects[index].clone();
        self.confirm_project_delete(project);
    }

    fn confirm_project_delete(&mut self, project: Project) {
        self.open_delete_confirmation(DeleteTarget::Project {
            project_id: project.id,
            project_name: project.name,
        });
    }

    fn confirm_selected_session_delete(&mut self, project_id: &str) {
        let Some(position) = self.session_selected.checked_sub(1) else {
            return;
        };
        let sessions = self.filtered_session_indices(project_id);
        let Some(index) = sessions.get(position).copied() else {
            return;
        };
        self.view = View::ConfirmDelete(DeleteTarget::Session {
            project_id: project_id.to_owned(),
            session_name: self.state.sessions[index].name.clone(),
            mode: self.state.sessions[index].mode,
        });
    }

    fn select_previous(&mut self) {
        let count = self.row_count();
        match self.focused_pane {
            FocusedPane::Projects => {
                let previous_project = self.active_project_id();
                self.project_selected = if self.project_selected == 0 {
                    count.saturating_sub(1)
                } else {
                    self.project_selected - 1
                };
                if previous_project != self.active_project_id() {
                    self.reset_session_cursor();
                }
            }
            FocusedPane::Sessions => {
                self.session_selected = if self.session_selected == 0 {
                    count.saturating_sub(1)
                } else {
                    self.session_selected - 1
                };
            }
        }
    }

    fn select_next(&mut self) {
        let count = self.row_count();
        if count > 0 {
            match self.focused_pane {
                FocusedPane::Projects => {
                    let previous_project = self.active_project_id();
                    self.project_selected = (self.project_selected + 1) % count;
                    if previous_project != self.active_project_id() {
                        self.reset_session_cursor();
                    }
                }
                FocusedPane::Sessions => {
                    self.session_selected = (self.session_selected + 1) % count;
                }
            }
        }
    }

    fn row_count(&self) -> usize {
        match (&self.view, self.focused_pane) {
            (View::Browser, FocusedPane::Projects) => self.filtered_project_indices().len() + 1,
            (View::Browser, FocusedPane::Sessions) => self
                .active_project_id()
                .map(|project_id| self.filtered_session_indices(&project_id).len() + 1)
                .unwrap_or(0),
            (
                View::AddProject(_)
                | View::NewSession { .. }
                | View::ContextMenu { .. }
                | View::ConfirmDelete(_),
                _,
            ) => 1,
        }
    }

    fn filtered_project_indices(&self) -> Vec<usize> {
        let filter = self.project_filter.to_lowercase();
        let mut projects = self
            .config
            .projects
            .iter()
            .enumerate()
            .filter(|(_, project)| {
                filter.is_empty()
                    || project.name.to_lowercase().contains(&filter)
                    || project
                        .path
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&filter)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        projects.sort_by_key(|index| {
            !self
                .config
                .pins
                .project_is_pinned(&self.config.projects[*index].id)
        });
        projects
    }

    fn filtered_session_indices(&self, project_id: &str) -> Vec<usize> {
        let filter = self.session_filter.to_lowercase();
        let mut sessions = self
            .state
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| {
                session.project_id == project_id
                    && (filter.is_empty() || session.name.to_lowercase().contains(&filter))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        sessions.sort_by_key(|index| {
            let session = &self.state.sessions[*index];
            (
                !self
                    .config
                    .pins
                    .session_is_pinned(&session.project_id, &session.name),
                std::cmp::Reverse(session.last_used_at_ms),
            )
        });
        sessions
    }

    fn draw(&mut self, frame: &mut Frame) {
        let theme = self.theme;
        frame.render_widget(
            Block::new().style(Style::default().bg(theme.canvas).fg(theme.primary_text)),
            frame.area(),
        );
        match self.view.clone() {
            View::Browser => self.draw_browser(frame),
            View::NewSession { input, mode, .. } => {
                self.draw_browser(frame);
                self.draw_new_session(frame, frame.area(), &input, mode);
            }
            View::AddProject(draft) => {
                self.draw_browser(frame);
                self.draw_add_project(frame, frame.area(), &draft);
            }
            View::ContextMenu {
                target,
                column,
                row,
                selected,
            } => {
                self.draw_browser(frame);
                self.draw_context_menu(frame, &target, column, row, selected);
            }
            View::ConfirmDelete(target) => {
                self.draw_browser(frame);
                self.draw_delete_confirmation(frame, frame.area(), &target);
            }
        }
        if let Some(error) = &self.error {
            self.draw_error(frame, error);
        }
    }

    fn draw_browser(&mut self, frame: &mut Frame) {
        let layout = UiLayout::new(frame.area());
        let project_id = self.active_project_id();
        self.draw_projects(frame, layout, project_id.as_deref());
        self.draw_sessions(frame, layout, project_id.as_deref());
    }

    fn draw_projects(
        &mut self,
        frame: &mut Frame,
        layout: UiLayout,
        active_project_id: Option<&str>,
    ) {
        let theme = self.theme;
        let focused = self.focused_pane == FocusedPane::Projects;
        let border_color = if focused { theme.accent } else { theme.border };
        let panel = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(Line::from(Span::styled(
                " Projects ",
                Style::default()
                    .fg(theme.primary_text)
                    .add_modifier(Modifier::BOLD),
            )))
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(theme.panel).fg(theme.primary_text));
        frame.render_widget(panel, layout.projects_panel);

        let search_text = if self.searching && focused {
            format!("/  {}_", self.project_filter)
        } else if !self.project_filter.is_empty() {
            format!("/  {}", self.project_filter)
        } else {
            "/  Search projects...".to_owned()
        };
        frame.render_widget(
            Paragraph::new(search_text)
                .style(Style::default().fg(if self.searching && focused {
                    theme.primary_text
                } else {
                    theme.secondary_text
                }))
                .block(
                    Block::new()
                        .borders(Borders::BOTTOM)
                        .border_style(Style::default().fg(theme.divider)),
                ),
            layout.project_search,
        );

        let add_selected = focused && self.project_selected == 0;
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("+  ", Style::default().fg(theme.accent)),
                Span::styled(
                    "Add project",
                    Style::default().fg(if add_selected {
                        theme.primary_text
                    } else {
                        theme.secondary_text
                    }),
                ),
            ]))
            .style(if add_selected {
                Style::default().bg(theme.selected_surface)
            } else {
                Style::default()
            })
            .block(
                Block::new()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(theme.divider)),
            ),
            layout.add_project,
        );

        let filtered = self.filtered_project_indices();
        let rows = filtered.iter().map(|index| {
            let project = &self.config.projects[*index];
            let pinned = self.config.pins.project_is_pinned(&project.id);
            let session_count = self
                .state
                .sessions
                .iter()
                .filter(|session| session.project_id == project.id)
                .count();
            Row::new(vec![
                Cell::from(Line::from(vec![
                    Span::styled(
                        if pinned { "◆ " } else { "  " },
                        Style::default().fg(theme.accent),
                    ),
                    Span::styled(
                        project.name.clone(),
                        Style::default()
                            .fg(theme.primary_text)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])),
                Cell::from(Span::styled(
                    project.agent.clone(),
                    Style::default().fg(theme.secondary_text),
                )),
                Cell::from(Span::styled(
                    session_count.to_string(),
                    Style::default().fg(theme.primary_text),
                )),
            ])
            .height(ROW_HEIGHT)
        });
        let selected = active_project_id.and_then(|id| {
            filtered
                .iter()
                .position(|index| self.config.projects[*index].id == id)
        });
        let table = Table::new(
            rows,
            [
                Constraint::Min(12),
                Constraint::Length(8),
                Constraint::Length(3),
            ],
        )
        .column_spacing(1)
        .style(Style::default().bg(theme.panel).fg(theme.primary_text))
        .row_highlight_style(
            Style::default()
                .bg(theme.selected_surface)
                .fg(theme.primary_text),
        )
        .highlight_symbol(Span::styled("▌ ", Style::default().fg(theme.accent)))
        .highlight_spacing(HighlightSpacing::Always);
        let mut state = TableState::default()
            .with_selected(selected)
            .with_offset(self.project_offset);
        frame.render_stateful_widget(table, layout.project_rows, &mut state);
        self.project_offset = state.offset();
    }

    fn draw_sessions(&mut self, frame: &mut Frame, layout: UiLayout, project_id: Option<&str>) {
        let theme = self.theme;
        let focused = self.focused_pane == FocusedPane::Sessions;
        let border_color = if focused { theme.accent } else { theme.border };
        let project =
            project_id.and_then(|id| self.config.projects.iter().find(|project| project.id == id));
        let title = project.map_or_else(
            || {
                Line::from(Span::styled(
                    " Sessions ",
                    Style::default().fg(theme.primary_text),
                ))
            },
            |project| {
                Line::from(vec![
                    Span::styled(
                        format!(" {} ", project.name),
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "/ Sessions ",
                        Style::default()
                            .fg(theme.primary_text)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])
            },
        );
        let panel = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(title)
            .border_style(Style::default().fg(border_color))
            .style(Style::default().bg(theme.panel).fg(theme.primary_text));
        frame.render_widget(panel, layout.sessions_panel);

        let searching_sessions = self.searching && focused;
        frame.render_widget(
            Paragraph::new(if searching_sessions {
                format!("/  {}_", self.session_filter)
            } else if !self.session_filter.is_empty() {
                format!("/  {}", self.session_filter)
            } else {
                "/  Search sessions...".to_owned()
            })
            .style(Style::default().fg(if searching_sessions {
                theme.primary_text
            } else {
                theme.muted_text
            }))
            .block(
                Block::new()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(theme.divider)),
            ),
            layout.session_search,
        );
        let new_selected = focused && self.session_selected == 0;
        let new_session_style = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD);
        let new_session_style = if new_selected {
            new_session_style.add_modifier(Modifier::UNDERLINED)
        } else {
            new_session_style
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("+  ", Style::default().fg(theme.accent)),
                Span::styled("New session", new_session_style),
            ]))
            .alignment(Alignment::Right)
            .block(
                Block::new()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(theme.divider)),
            ),
            layout.new_session,
        );

        let Some(project_id) = project_id else {
            frame.render_widget(
                Paragraph::new("Select a project")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(theme.muted_text)),
                layout.session_rows,
            );
            return;
        };
        let session_indices = self.filtered_session_indices(project_id);
        let rows = session_indices.iter().map(|index| {
            let session = &self.state.sessions[*index];
            let pinned = self
                .config
                .pins
                .session_is_pinned(&session.project_id, &session.name);
            let status = self.session_status(session);
            let (symbol, label, color) = status_presentation(theme, &status);
            Row::new(vec![
                Cell::from(Line::from(vec![
                    Span::styled(
                        if pinned { "◆  " } else { "⑂  " },
                        Style::default().fg(if pinned {
                            theme.accent
                        } else {
                            theme.secondary_text
                        }),
                    ),
                    Span::styled(
                        session.name.clone(),
                        Style::default()
                            .fg(theme.primary_text)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])),
                Cell::from(Line::from(vec![
                    Span::styled(format!("{symbol}  "), Style::default().fg(color)),
                    Span::styled(label, Style::default().fg(color)),
                ])),
                Cell::from(Span::styled(
                    relative_age(session.last_used_at_ms, now_ms()),
                    Style::default().fg(theme.secondary_text),
                )),
            ])
            .height(ROW_HEIGHT)
        });
        let selected = self.displayed_session_position(!session_indices.is_empty());
        let table = Table::new(
            rows,
            [
                Constraint::Min(18),
                Constraint::Length(13),
                Constraint::Length(6),
            ],
        )
        .column_spacing(1)
        .style(Style::default().bg(theme.panel).fg(theme.primary_text))
        .row_highlight_style(
            Style::default()
                .bg(theme.selected_surface)
                .fg(theme.primary_text),
        )
        .highlight_symbol(Span::styled("▌ ", Style::default().fg(theme.accent)))
        .highlight_spacing(HighlightSpacing::Always);
        let mut state = TableState::default()
            .with_selected(selected)
            .with_offset(self.session_offset);
        frame.render_stateful_widget(table, layout.session_rows, &mut state);
        self.session_offset = state.offset();
    }

    fn draw_context_menu(
        &self,
        frame: &mut Frame,
        target: &DeleteTarget,
        column: u16,
        row: u16,
        selected: usize,
    ) {
        let theme = self.theme;
        let layout = ContextMenuLayout::new(frame.area(), column, row);
        let pinned = match target {
            DeleteTarget::Project { project_id, .. } => {
                self.config.pins.project_is_pinned(project_id)
            }
            DeleteTarget::Session {
                project_id,
                session_name,
                ..
            } => self.config.pins.session_is_pinned(project_id, session_name),
        };
        frame.render_widget(Clear, layout.popup);
        frame.render_widget(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border))
                .style(Style::default().bg(theme.panel)),
            layout.popup,
        );
        for (index, (area, label, color)) in [
            (
                layout.pin,
                if pinned { "Unpin" } else { "Pin" },
                theme.primary_text,
            ),
            (layout.remove, "Remove", theme.danger),
        ]
        .into_iter()
        .enumerate()
        {
            frame.render_widget(
                Paragraph::new(format!("  {label}")).style(Style::default().fg(color).bg(
                    if selected == index {
                        theme.selected_surface
                    } else {
                        theme.panel
                    },
                )),
                area,
            );
        }
    }

    fn draw_new_session(
        &self,
        frame: &mut Frame,
        area: ratatui::layout::Rect,
        input: &str,
        mode: SessionMode,
    ) {
        let theme = self.theme;
        let layout = NewSessionLayout::new(area);
        frame.render_widget(Clear, layout.popup);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(Span::styled(
                " New session ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Center)
            .border_style(Style::default().fg(theme.accent))
            .style(Style::default().bg(theme.panel).fg(theme.primary_text));
        frame.render_widget(block, layout.popup);
        frame.render_widget(
            Paragraph::new(match mode {
                SessionMode::Worktree => vec![
                    Line::from("Creates an isolated detached worktree."),
                    Line::from("Starts in detached HEAD state."),
                ],
                SessionMode::Local => vec![
                    Line::from("Uses the project directory directly."),
                    Line::from("Sessions share files but use separate agent tabs."),
                ],
            })
            .alignment(Alignment::Center),
            layout.description,
        );
        frame.render_widget(
            Paragraph::new(Span::styled("Mode", Style::default().fg(theme.muted_text)))
                .alignment(Alignment::Center),
            Rect::new(
                layout.mode_worktree.x.saturating_sub(7),
                layout.mode_worktree.y,
                7,
                layout.mode_worktree.height,
            ),
        );
        self.draw_mode_option(
            frame,
            layout.mode_worktree,
            "Worktree",
            mode == SessionMode::Worktree,
        );
        self.draw_mode_option(
            frame,
            layout.mode_local,
            "Local",
            mode == SessionMode::Local,
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Title  ", Style::default().fg(theme.muted_text)),
                Span::styled(
                    format!("{input}_"),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
            .alignment(Alignment::Center),
            layout.title,
        );
        let create_style = if input.trim().is_empty() {
            Style::default().fg(theme.muted_text)
        } else {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        };
        frame.render_widget(
            Paragraph::new(" Create ")
                .alignment(Alignment::Center)
                .style(create_style)
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(create_style),
                ),
            layout.create,
        );
        frame.render_widget(
            Paragraph::new(" Cancel ")
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.secondary_text))
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(theme.border)),
                ),
            layout.cancel,
        );
    }

    fn draw_mode_option(&self, frame: &mut Frame, area: Rect, label: &str, selected: bool) {
        let theme = self.theme;
        let style = if selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.secondary_text)
        };
        let label = if selected {
            format!("[{label}]")
        } else {
            format!(" {label} ")
        };
        frame.render_widget(
            Paragraph::new(label)
                .alignment(Alignment::Center)
                .style(style),
            area,
        );
    }

    fn draw_delete_confirmation(&self, frame: &mut Frame, area: Rect, target: &DeleteTarget) {
        let theme = self.theme;
        let layout = DeleteConfirmationLayout::new(area);
        let (title, lines) = match target {
            DeleteTarget::Project { project_name, .. } => (
                " Delete project ",
                vec![
                    Line::from(Span::styled(
                        project_name.clone(),
                        Style::default()
                            .fg(theme.primary_text)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from("Removes it from Switchyard only."),
                    Line::from("Project files stay untouched."),
                ],
            ),
            DeleteTarget::Session {
                session_name, mode, ..
            } => (
                " Delete session ",
                match mode {
                    SessionMode::Worktree => vec![
                        Line::from(Span::styled(
                            session_name.clone(),
                            Style::default()
                                .fg(theme.primary_text)
                                .add_modifier(Modifier::BOLD),
                        )),
                        Line::from(""),
                        Line::from("Deletes its Git worktree and session record."),
                        Line::from("Dirty files or unbranched commits prevent deletion."),
                        Line::from("Open sessions must be closed first."),
                    ],
                    SessionMode::Local => vec![
                        Line::from(Span::styled(
                            session_name.clone(),
                            Style::default()
                                .fg(theme.primary_text)
                                .add_modifier(Modifier::BOLD),
                        )),
                        Line::from(""),
                        Line::from("Removes the session record only."),
                        Line::from("Project files stay untouched."),
                        Line::from("Its agent tab must be closed first."),
                    ],
                },
            ),
        };
        frame.render_widget(Clear, layout.popup);
        frame.render_widget(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    title,
                    Style::default()
                        .fg(theme.danger)
                        .add_modifier(Modifier::BOLD),
                ))
                .border_style(Style::default().fg(theme.danger))
                .style(Style::default().bg(theme.panel).fg(theme.primary_text)),
            layout.popup,
        );
        frame.render_widget(Paragraph::new(lines), layout.body);
        let delete_style = Style::default()
            .fg(theme.danger)
            .add_modifier(Modifier::BOLD);
        frame.render_widget(
            Paragraph::new(" Delete ").style(delete_style).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(delete_style),
            ),
            layout.delete,
        );
        frame.render_widget(
            Paragraph::new(" Cancel ")
                .style(Style::default().fg(theme.secondary_text))
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(theme.border)),
                ),
            layout.cancel,
        );
    }

    fn draw_add_project(&mut self, frame: &mut Frame, area: Rect, draft: &ProjectDraft) {
        let theme = self.theme;
        let layout = AddProjectLayout::new(area);
        frame.render_widget(Clear, layout.popup);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(Span::styled(
                " Add project ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ))
            .border_style(Style::default().fg(theme.accent))
            .style(Style::default().bg(theme.panel).fg(theme.primary_text));
        frame.render_widget(block, layout.popup);

        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("Folder  ", Style::default().fg(theme.muted_text)),
                    Span::styled("/  ", Style::default().fg(theme.accent)),
                    if draft.filter.is_empty() {
                        Span::styled("type to filter...", Style::default().fg(theme.muted_text))
                    } else {
                        Span::styled(
                            format!("{}_", draft.filter),
                            Style::default().fg(theme.primary_text),
                        )
                    },
                ]),
                Line::from(Span::styled(
                    draft.current_dir.display().to_string(),
                    Style::default().fg(theme.primary_text),
                )),
            ])
            .block(
                Block::new()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(theme.divider)),
            ),
            layout.path,
        );

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Defaults  ", Style::default().fg(theme.muted_text)),
                Span::styled(
                    format!("{DEFAULT_AGENT} · {BASE_BRANCH_LABEL} · {DEFAULT_SESSION_MODE}"),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
            .block(
                Block::new()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(theme.divider)),
            ),
            layout.defaults,
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    if draft.show_hidden { "● " } else { "○ " },
                    Style::default().fg(theme.accent),
                ),
                Span::styled(
                    if draft.show_hidden {
                        "Hide hidden"
                    } else {
                        "Show hidden"
                    },
                    Style::default().fg(theme.secondary_text),
                ),
            ]))
            .alignment(Alignment::Right)
            .block(
                Block::new()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(theme.divider)),
            ),
            layout.hidden_toggle,
        );

        let mut rows = vec![
            Row::new(vec![Cell::from(Line::from(vec![
                Span::styled("✓  ", Style::default().fg(theme.accent)),
                Span::styled(
                    "Add this folder",
                    Style::default()
                        .fg(theme.primary_text)
                        .add_modifier(Modifier::BOLD),
                ),
            ]))])
            .height(DIRECTORY_ROW_HEIGHT),
        ];
        if draft.current_dir.parent().is_some() {
            rows.push(
                Row::new(vec![Cell::from(Line::from(vec![
                    Span::styled("↑  ", Style::default().fg(theme.secondary_text)),
                    Span::styled("..", Style::default().fg(theme.secondary_text)),
                ]))])
                .height(DIRECTORY_ROW_HEIGHT),
            );
        }
        rows.extend(draft.visible_directories().map(|path| {
            Row::new(vec![Cell::from(Line::from(vec![
                Span::styled("▸  ", Style::default().fg(theme.muted_text)),
                Span::styled(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    Style::default().fg(theme.primary_text),
                ),
            ]))])
            .height(DIRECTORY_ROW_HEIGHT)
        }));
        let table = Table::new(rows, [Constraint::Percentage(100)])
            .style(Style::default().bg(theme.panel).fg(theme.primary_text))
            .row_highlight_style(
                Style::default()
                    .bg(theme.selected_surface)
                    .fg(theme.primary_text),
            )
            .highlight_symbol(Span::styled("▌ ", Style::default().fg(theme.accent)))
            .highlight_spacing(HighlightSpacing::Always);
        let mut state = TableState::default()
            .with_selected(Some(draft.selected))
            .with_offset(self.directory_offset);
        frame.render_stateful_widget(table, layout.directory_rows, &mut state);
        self.directory_offset = state.offset();
    }

    fn draw_error(&self, frame: &mut Frame, error: &str) {
        let theme = self.theme;
        let area = frame.area();
        let width = u16::try_from(error.chars().count())
            .unwrap_or(u16::MAX)
            .saturating_add(4)
            .min(area.width.saturating_sub(2))
            .max(12.min(area.width));
        let height = 3.min(area.height);
        let rect = Rect::new(
            area.right().saturating_sub(width).saturating_sub(1),
            area.bottom().saturating_sub(height).saturating_sub(1),
            width,
            height,
        );
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(error)
                .style(Style::default().bg(theme.panel).fg(theme.danger))
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .title(" Error ")
                        .border_style(Style::default().fg(theme.danger)),
                ),
            rect,
        );
    }

    fn session_status(&self, session: &Session) -> String {
        let Some(workspace) = self
            .snapshot
            .workspaces
            .iter()
            .find(|workspace| same_path(&workspace.checkout_path, &session.worktree_path))
        else {
            return if session.worktree_path.exists() {
                "dormant".into()
            } else {
                "missing".into()
            };
        };
        let Some(project) = self
            .config
            .projects
            .iter()
            .find(|project| project.id == session.project_id)
        else {
            return "open".into();
        };
        let expected_name = agent_name(project, &session.name);
        self.snapshot
            .agents
            .iter()
            .find(|agent| {
                agent.workspace_id == workspace.id
                    && agent.name.as_deref() == Some(expected_name.as_str())
                    && agent.kind.as_deref() == Some(project.agent.as_str())
            })
            .map(|agent| agent.status.clone())
            .unwrap_or_else(|| "open".into())
    }
}

fn contains(rect: Rect, mouse: MouseEvent) -> bool {
    mouse.column >= rect.x
        && mouse.column < rect.right()
        && mouse.row >= rect.y
        && mouse.row < rect.bottom()
}

fn table_row_at(rect: Rect, mouse: MouseEvent, offset: usize) -> Option<usize> {
    let relative_row = mouse.row.checked_sub(rect.y)?;
    let fully_rendered_height = (rect.height / ROW_HEIGHT) * ROW_HEIGHT;
    (relative_row < fully_rendered_height).then(|| offset + usize::from(relative_row / ROW_HEIGHT))
}

fn status_presentation(theme: Theme, status: &str) -> (&'static str, String, Color) {
    match status {
        "working" => ("●", "WORKING".into(), theme.working),
        "done" => ("✓", "DONE".into(), theme.success),
        "idle" => ("✓", "IDLE".into(), theme.success),
        "dormant" => ("◷", "DORMANT".into(), theme.info),
        "blocked" => ("⊖", "BLOCKED".into(), theme.danger),
        "missing" => ("⊖", "MISSING".into(), theme.danger),
        "open" => ("○", "OPEN".into(), theme.accent),
        other => ("○", other.to_uppercase(), theme.info),
    }
}

fn relative_age(last_used_at_ms: u64, current_ms: u64) -> String {
    let elapsed_seconds = current_ms.saturating_sub(last_used_at_ms) / 1_000;
    if elapsed_seconds < 60 {
        "now".into()
    } else if elapsed_seconds < 3_600 {
        format!("{}m", elapsed_seconds / 60)
    } else if elapsed_seconds < 86_400 {
        format!("{}h", elapsed_seconds / 3_600)
    } else {
        format!("{}d", elapsed_seconds / 86_400)
    }
}

fn slug(input: &str) -> String {
    let mut output = String::new();
    let mut previous_dash = false;
    for character in input.trim().chars() {
        let character = if character.is_ascii_alphanumeric() {
            character.to_ascii_lowercase()
        } else {
            '-'
        };
        if character == '-' && (output.is_empty() || previous_dash) {
            continue;
        }
        output.push(character);
        previous_dash = character == '-';
    }
    output.trim_end_matches('-').to_owned()
}

fn repair_config_base_branches(config: &mut Config) -> (bool, Option<String>) {
    let mut repaired = false;
    let mut errors = Vec::new();
    for project in &mut config.projects {
        match repair_base_branch(project) {
            Ok(changed) => repaired |= changed,
            Err(error) => errors.push(format!(
                "Could not detect base branch for {}: {error:#}",
                project.name
            )),
        }
    }
    (repaired, (!errors.is_empty()).then(|| errors.join(" · ")))
}

pub fn run(store: &Store, herdr: &CliHerdr) -> Result<()> {
    let (config, repair_warning) = store.update_config(|config, _state| {
        let (_, warning) = repair_config_base_branches(config);
        Ok(warning)
    })?;
    let snapshot = store.update_state(|state| {
        let snapshot = herdr.snapshot()?;
        sync_agent_sessions(state, &snapshot, &config.projects);
        Ok(snapshot)
    })?;
    let state = store.load_state()?;
    let mut picker = Picker::new(config, state, snapshot);
    picker.error = repair_warning;

    let mut restore = TerminalRestore::enter()?;
    let output = stdout();
    let backend = CrosstermBackend::new(output);
    let mut terminal = Terminal::new(backend).context("create terminal")?;
    let result = run_loop(&mut terminal, &mut picker, store, herdr);
    drop(terminal);
    let cleanup = restore.restore().context("restore terminal state");
    match result {
        Err(error) => Err(error),
        Ok(()) => cleanup,
    }
}

#[derive(Default)]
struct TerminalRestore {
    raw_mode: bool,
    alternate_screen: bool,
    mouse_capture: bool,
}

impl TerminalRestore {
    fn enter() -> Result<Self> {
        let mut restore = Self::default();
        enable_raw_mode().context("enable terminal raw mode")?;
        restore.raw_mode = true;

        let mut output = stdout();
        execute!(output, EnterAlternateScreen).context("enter alternate screen")?;
        restore.alternate_screen = true;
        restore
            .enable_mouse_capture(&mut output)
            .context("enable terminal mouse capture")?;
        Ok(restore)
    }

    fn enable_mouse_capture<W: Write>(&mut self, output: &mut W) -> io::Result<()> {
        self.mouse_capture = true;
        execute!(output, EnableMouseCapture)
    }

    fn disable_mouse_capture<W: Write>(&mut self, output: &mut W) -> io::Result<()> {
        let result = execute!(output, DisableMouseCapture);
        if result.is_ok() {
            self.mouse_capture = false;
        }
        result
    }

    fn restore(&mut self) -> io::Result<()> {
        let mut first_error = None;
        if self.mouse_capture {
            let mut output = stdout();
            if let Err(error) = self.disable_mouse_capture(&mut output) {
                first_error = Some(error);
            }
        }
        if self.alternate_screen {
            let mut output = stdout();
            if let Err(error) = execute!(output, Show)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
            if let Err(error) = execute!(output, LeaveAlternateScreen)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
            self.alternate_screen = false;
        }
        if self.raw_mode {
            if let Err(error) = disable_raw_mode()
                && first_error.is_none()
            {
                first_error = Some(error);
            }
            self.raw_mode = false;
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub fn sync<H: Herdr>(store: &Store, herdr: &H) -> Result<()> {
    let config = store.load_config()?;
    store.update_state(|state| {
        let snapshot = herdr.snapshot()?;
        sync_agent_sessions(state, &snapshot, &config.projects);
        Ok(())
    })
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    picker: &mut Picker,
    store: &Store,
    herdr: &CliHerdr,
) -> Result<()> {
    loop {
        let frame = terminal.draw(|frame| picker.draw(frame))?;
        let intent = match event::read().context("read terminal input")? {
            event::Event::Key(key) if key.kind == KeyEventKind::Press => picker.handle_key(key),
            event::Event::Mouse(mouse) => picker.handle_mouse(mouse, frame.area),
            _ => continue,
        };
        match intent {
            Intent::None => {}
            Intent::Quit => return Ok(()),
            Intent::AddProject(project) => match store.update_config(|config, _state| {
                let project = normalize_project(project, config)?;
                config.projects.push(project.clone());
                Ok(project)
            }) {
                Ok((config, project)) => picker.project_added(config, &project.id),
                Err(error) => picker.error = Some(format!("{error:#}")),
            },
            Intent::ActivateSession {
                project_id,
                session_name,
            } => {
                let result = store.update_project_state(&project_id, |project, state| {
                    activate_existing(herdr, project, state, &session_name, now_ms())
                });
                match result {
                    Ok(_) => return Ok(()),
                    Err(error) => {
                        picker.state = store.load_state()?;
                        picker.error = Some(format!("{error:#}"));
                    }
                }
            }
            Intent::CreateSession {
                project_id,
                session_name,
                mode,
            } => {
                let result = store.update_project_state(&project_id, |project, state| {
                    create_session(herdr, project, state, &session_name, mode, now_ms())
                });
                match result {
                    Ok(_) => return Ok(()),
                    Err(error) => {
                        picker.state = store.load_state()?;
                        picker.error = Some(format!("{error:#}"));
                    }
                }
            }
            Intent::DeleteProject { project_id } => match store.remove_project(&project_id) {
                Ok(config) => {
                    picker.config = config;
                    picker.project_deleted(&project_id);
                }
                Err(error) => picker.error = Some(format!("{error:#}")),
            },
            Intent::DeleteSession {
                project_id,
                session_name,
            } => {
                let result = store.update_project_state(&project_id, |project, state| {
                    delete_session(herdr, project, state, &session_name)
                });
                picker.state = store.load_state()?;
                match result {
                    Ok(()) => {
                        picker.session_deleted();
                        match store.update_config(|config, _state| {
                            config.pins.remove_session(&project_id, &session_name);
                            Ok(())
                        }) {
                            Ok((config, ())) => picker.config = config,
                            Err(error) => {
                                picker.error = Some(format!(
                                    "Session removed, but its pin could not be cleaned up: {error:#}"
                                ));
                            }
                        }
                    }
                    Err(error) => picker.error = Some(format!("{error:#}")),
                }
            }
            Intent::ToggleProjectPin { project_id } => {
                match store.update_config(|config, _state| {
                    config.pins.toggle_project(&project_id);
                    Ok(())
                }) {
                    Ok((config, ())) => picker.project_pin_toggled(config, &project_id),
                    Err(error) => picker.error = Some(format!("{error:#}")),
                }
            }
            Intent::ToggleSessionPin {
                project_id,
                session_name,
            } => match store.update_config(|config, _state| {
                config.pins.toggle_session(&project_id, &session_name);
                Ok(())
            }) {
                Ok((config, ())) => picker.session_pin_toggled(config, &project_id, &session_name),
                Err(error) => picker.error = Some(format!("{error:#}")),
            },
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        path::{Path, PathBuf},
        process::Command,
        time::Duration,
    };

    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn picker() -> Picker {
        Picker::new(
            Config {
                version: 1,
                ui: Default::default(),
                pins: Default::default(),
                projects: vec![Project {
                    id: "demo".into(),
                    name: "Demo".into(),
                    path: PathBuf::from("/repos/demo"),
                    agent: "codex".into(),
                    base_branch: "main".into(),
                    agent_args: Vec::new(),
                }],
            },
            State::default(),
            RuntimeSnapshot::default(),
        )
    }

    fn picker_with_session() -> Picker {
        let mut picker = picker();
        picker.state.sessions.push(Session {
            project_id: "demo".into(),
            name: "feat/one".into(),
            mode: SessionMode::Worktree,
            worktree_path: PathBuf::from("/worktrees/demo/feat-one"),
            pending_temporary_branch: None,
            created_at_ms: 1,
            last_used_at_ms: 2,
            agent_session: None,
            tab_id: None,
            tab_namespace: None,
        });
        picker
    }

    fn picker_with_many_sessions() -> Picker {
        let mut picker = picker();
        for index in 0..12 {
            picker.state.sessions.push(Session {
                project_id: "demo".into(),
                name: format!("feat/{index:02}"),
                mode: SessionMode::Worktree,
                worktree_path: PathBuf::from(format!("/worktrees/demo/feat-{index:02}")),
                pending_temporary_branch: None,
                created_at_ms: index,
                last_used_at_ms: index,
                agent_session: None,
                tab_id: None,
                tab_namespace: None,
            });
        }
        picker
    }

    fn left_click(rect: Rect) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + rect.width.saturating_sub(1) / 2,
            row: rect.y + rect.height.saturating_sub(1) / 2,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn right_click(rect: Rect) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: rect.x + rect.width.saturating_sub(1) / 2,
            row: rect.y + rect.height.saturating_sub(1) / 2,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn initialize_git_repository(path: &Path, branch: &str) {
        fs::create_dir_all(path).unwrap();
        let initialized = Command::new("git")
            .args(["-C"])
            .arg(path)
            .arg("init")
            .output()
            .unwrap();
        assert!(initialized.status.success());
        let selected = Command::new("git")
            .args(["-C"])
            .arg(path)
            .args(["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")])
            .output()
            .unwrap();
        assert!(selected.status.success());
        let committed = Command::new("git")
            .args(["-C"])
            .arg(path)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@localhost")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@localhost")
            .args(["commit", "--allow-empty", "--no-gpg-sign", "-m", "existing"])
            .output()
            .unwrap();
        assert!(committed.status.success());
    }

    #[test]
    fn enter_on_a_project_opens_its_session_list() {
        let mut picker = picker();

        assert_eq!(picker.handle_key(key(KeyCode::Enter)), Intent::None);

        assert_eq!(picker.view, View::Browser);
        assert_eq!(picker.focused_pane, FocusedPane::Sessions);
        assert_eq!(picker.session_selected, 0);
    }

    #[test]
    fn new_session_form_returns_a_create_intent() {
        let mut picker = picker();
        picker.handle_key(key(KeyCode::Enter));
        picker.handle_key(key(KeyCode::Char('n')));
        for character in "feat/one".chars() {
            picker.handle_key(key(KeyCode::Char(character)));
        }

        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            Intent::CreateSession {
                project_id: "demo".into(),
                session_name: "feat/one".into(),
                mode: SessionMode::Worktree,
            }
        );
    }

    #[test]
    fn delete_key_confirms_the_selected_project_before_deleting() {
        let mut picker = picker();

        assert_eq!(picker.handle_key(key(KeyCode::Delete)), Intent::None);
        assert!(matches!(
            picker.view,
            View::ConfirmDelete(DeleteTarget::Project { ref project_id, .. })
                if project_id == "demo"
        ));
        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            Intent::DeleteProject {
                project_id: "demo".into(),
            }
        );
    }

    #[test]
    fn a_project_with_sessions_must_delete_its_sessions_first() {
        let mut picker = picker_with_session();

        assert_eq!(picker.handle_key(key(KeyCode::Delete)), Intent::None);

        assert_eq!(picker.view, View::Browser);
        assert!(
            picker
                .error
                .as_deref()
                .unwrap()
                .contains("Delete its sessions first")
        );
    }

    #[test]
    fn backspace_confirms_the_selected_session_and_escape_cancels() {
        let mut picker = picker_with_session();
        picker.handle_key(key(KeyCode::Enter));
        picker.handle_key(key(KeyCode::Down));

        assert_eq!(picker.handle_key(key(KeyCode::Backspace)), Intent::None);
        assert!(matches!(
            picker.view,
            View::ConfirmDelete(DeleteTarget::Session { ref session_name, .. })
                if session_name == "feat/one"
        ));
        assert_eq!(picker.handle_key(key(KeyCode::Esc)), Intent::None);
        assert_eq!(picker.view, View::Browser);
    }

    #[test]
    fn right_clicking_a_session_opens_its_context_menu() {
        let mut picker = picker_with_session();
        picker.handle_key(key(KeyCode::Enter));
        let area = Rect::new(0, 0, 120, 36);
        let layout = UiLayout::new(area);
        let first_session_row = Rect::new(
            layout.session_rows.x,
            layout.session_rows.y,
            layout.session_rows.width,
            ROW_HEIGHT,
        );

        assert_eq!(
            picker.handle_mouse_at(
                right_click(first_session_row),
                area,
                std::time::Instant::now(),
            ),
            Intent::None
        );
        assert!(matches!(
            picker.view,
            View::ContextMenu {
                target: DeleteTarget::Session { ref session_name, .. },
                selected: 0,
                ..
            }
                if session_name == "feat/one"
        ));

        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Pin"));
        assert!(rendered.contains("Remove"));

        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            Intent::ToggleSessionPin {
                project_id: "demo".into(),
                session_name: "feat/one".into(),
            }
        );
    }

    #[test]
    fn context_menu_actions_can_be_clicked() {
        let mut picker = picker_with_session();
        picker.handle_key(key(KeyCode::Enter));
        let area = Rect::new(0, 0, 120, 36);
        let layout = UiLayout::new(area);
        let first_session_row = Rect::new(
            layout.session_rows.x,
            layout.session_rows.y,
            layout.session_rows.width,
            ROW_HEIGHT,
        );
        let click = right_click(first_session_row);
        picker.handle_mouse_at(click, area, std::time::Instant::now());
        let menu = ContextMenuLayout::new(area, click.column, click.row);

        assert_eq!(
            picker.handle_mouse_at(left_click(menu.pin), area, std::time::Instant::now()),
            Intent::ToggleSessionPin {
                project_id: "demo".into(),
                session_name: "feat/one".into(),
            }
        );

        picker.handle_mouse_at(click, area, std::time::Instant::now());
        assert_eq!(
            picker.handle_mouse_at(left_click(menu.remove), area, std::time::Instant::now()),
            Intent::None
        );
        assert!(matches!(
            picker.view,
            View::ConfirmDelete(DeleteTarget::Session { ref session_name, .. })
                if session_name == "feat/one"
        ));
    }

    #[test]
    fn right_clicking_a_project_opens_its_context_menu() {
        let mut picker = picker();
        let area = Rect::new(0, 0, 120, 36);
        let layout = UiLayout::new(area);
        let first_project_row = Rect::new(
            layout.project_rows.x,
            layout.project_rows.y,
            layout.project_rows.width,
            ROW_HEIGHT,
        );

        assert_eq!(
            picker.handle_mouse_at(
                right_click(first_project_row),
                area,
                std::time::Instant::now(),
            ),
            Intent::None
        );
        assert!(matches!(
            picker.view,
            View::ContextMenu {
                target: DeleteTarget::Project { ref project_id, .. },
                selected: 0,
                ..
            }
                if project_id == "demo"
        ));

        assert_eq!(picker.handle_key(key(KeyCode::Down)), Intent::None);
        assert_eq!(picker.handle_key(key(KeyCode::Enter)), Intent::None);
        assert!(matches!(
            picker.view,
            View::ConfirmDelete(DeleteTarget::Project { ref project_id, .. })
                if project_id == "demo"
        ));
    }

    #[test]
    fn pinned_projects_and_sessions_sort_before_unpinned_items() {
        let mut picker = picker_with_session();
        picker.config.projects.push(Project {
            id: "other".into(),
            name: "Other".into(),
            path: PathBuf::from("/repos/other"),
            agent: "pi".into(),
            base_branch: "main".into(),
            agent_args: Vec::new(),
        });
        picker.state.sessions.push(Session {
            project_id: "demo".into(),
            name: "older-pinned".into(),
            mode: SessionMode::Local,
            worktree_path: PathBuf::from("/repos/demo"),
            pending_temporary_branch: None,
            created_at_ms: 0,
            last_used_at_ms: 0,
            agent_session: None,
            tab_id: None,
            tab_namespace: None,
        });
        picker.config.pins.toggle_project("other");
        picker.config.pins.toggle_session("demo", "older-pinned");

        let projects = picker.filtered_project_indices();
        assert_eq!(picker.config.projects[projects[0]].id, "other");
        let sessions = picker.filtered_session_indices("demo");
        assert_eq!(picker.state.sessions[sessions[0]].name, "older-pinned");
    }

    #[test]
    fn delete_confirmation_draws_the_destructive_scope() {
        let mut picker = picker_with_session();
        picker.handle_key(key(KeyCode::Enter));
        picker.handle_key(key(KeyCode::Down));
        picker.handle_key(key(KeyCode::Delete));
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| picker.draw(frame)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Delete session"));
        assert!(rendered.contains("Deletes its Git worktree"));
        assert!(rendered.contains("Dirty files or unbranched commits prevent deletion"));
        assert!(rendered.contains("Open sessions must be closed first"));
    }

    #[test]
    fn new_session_is_a_centered_mode_dialog() {
        let mut picker = picker();
        picker.handle_key(key(KeyCode::Enter));
        picker.handle_key(key(KeyCode::Char('n')));
        let area = Rect::new(0, 0, 120, 36);
        let layout = NewSessionLayout::new(area);
        assert_eq!(layout.popup.width, 64);
        assert_eq!(layout.popup.height, 13);
        assert_eq!(layout.popup.x, 28);
        assert_eq!(layout.popup.y, 12);
        let inner = Block::bordered().inner(layout.popup);
        let mode_left = layout.mode_worktree.x.saturating_sub(7) - inner.x;
        let mode_right = inner.right() - layout.mode_local.right();
        let action_left = layout.create.x - inner.x;
        let action_right = inner.right() - layout.cancel.right();
        assert!(mode_left.abs_diff(mode_right) <= 1);
        assert!(action_left.abs_diff(action_right) <= 1);

        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        for row in [layout.description.y, layout.title.y] {
            let start = (row * area.width + inner.x) as usize;
            let end = start + inner.width as usize;
            let line = buffer.content()[start..end]
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            let content = line.trim();
            let left = line.find(content).unwrap();
            let right = line.len() - left - content.len();
            assert!(left.abs_diff(right) <= 1);
        }
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Projects"));
        assert!(rendered.contains("Creates an isolated detached worktree."));
        assert!(rendered.contains("Starts in detached HEAD state."));
        assert!(rendered.contains("[Worktree]"));
        assert!(rendered.contains("Local"));
        assert!(rendered.contains("Title"));
    }

    #[test]
    fn new_session_popup_is_centered_in_an_offset_odd_sized_viewport() {
        let area = Rect::new(7, 11, 91, 31);
        let popup = NewSessionLayout::new(area).popup;

        let left = popup.x - area.x;
        let right = area.right() - popup.right();
        let top = popup.y - area.y;
        let bottom = area.bottom() - popup.bottom();
        assert!(left.abs_diff(right) <= 1);
        assert!(top.abs_diff(bottom) <= 1);
    }

    #[test]
    fn arrow_key_selects_local_mode_for_a_new_session() {
        let mut picker = picker();
        picker.handle_key(key(KeyCode::Enter));
        picker.handle_key(key(KeyCode::Char('n')));
        picker.handle_key(key(KeyCode::Right));
        for character in "quick fix".chars() {
            picker.handle_key(key(KeyCode::Char(character)));
        }

        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            Intent::CreateSession {
                project_id: "demo".into(),
                session_name: "quick fix".into(),
                mode: SessionMode::Local,
            }
        );
    }

    #[test]
    fn mouse_selects_local_mode_for_a_new_session() {
        let mut picker = picker();
        picker.handle_key(key(KeyCode::Enter));
        picker.handle_key(key(KeyCode::Char('n')));
        let area = Rect::new(0, 0, 120, 36);
        let layout = NewSessionLayout::new(area);

        assert_eq!(
            picker.handle_mouse_at(
                left_click(layout.mode_local),
                area,
                std::time::Instant::now(),
            ),
            Intent::None
        );
        assert!(matches!(
            picker.view,
            View::NewSession {
                mode: SessionMode::Local,
                ..
            }
        ));
    }

    #[test]
    fn mouse_can_create_or_cancel_a_new_session() {
        let mut picker = picker();
        picker.handle_key(key(KeyCode::Enter));
        picker.handle_key(key(KeyCode::Char('n')));
        for character in "Improve login".chars() {
            picker.handle_key(key(KeyCode::Char(character)));
        }
        let area = Rect::new(0, 0, 120, 36);
        let layout = NewSessionLayout::new(area);

        assert_eq!(
            picker.handle_mouse_at(left_click(layout.create), area, std::time::Instant::now(),),
            Intent::CreateSession {
                project_id: "demo".into(),
                session_name: "Improve login".into(),
                mode: SessionMode::Worktree,
            }
        );

        picker.open_new_session("demo");
        assert_eq!(
            picker.handle_mouse_at(left_click(layout.cancel), area, std::time::Instant::now(),),
            Intent::None
        );
        assert_eq!(picker.view, View::Browser);
    }

    #[test]
    fn choosing_a_directory_creates_a_pi_main_worktree_project() {
        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("my-project");
        fs::create_dir(&project_path).unwrap();
        let project_path = fs::canonicalize(project_path).unwrap();
        let mut picker = picker();
        picker.open_add_project_at(root.path().to_path_buf());

        picker.handle_key(key(KeyCode::Down));
        picker.handle_key(key(KeyCode::Down));
        assert_eq!(picker.handle_key(key(KeyCode::Enter)), Intent::None);

        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            Intent::AddProject(Project {
                id: "my-project".into(),
                name: "my-project".into(),
                path: project_path,
                agent: "pi".into(),
                base_branch: "main".into(),
                agent_args: Vec::new(),
            })
        );
    }

    #[test]
    fn add_project_is_a_small_directory_picker_over_the_browser() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("not-a-directory.txt"), "ignored").unwrap();
        let mut picker = picker();
        picker.open_add_project_at(root.path().to_path_buf());
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| picker.draw(frame)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Projects"));
        assert!(rendered.contains("Add project"));
        assert!(rendered.contains("Add this folder"));
        assert!(rendered.contains("pi · auto branch · worktree"));
        assert!(!rendered.contains("[codex]"));
        assert!(!rendered.contains("Name"));
        assert!(!rendered.contains("Agent"));
        assert!(!rendered.contains("Base"));
        assert!(!rendered.contains("not-a-directory.txt"));
    }

    #[test]
    fn add_project_hides_dot_directories_until_the_toggle_is_clicked() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("visible")).unwrap();
        fs::create_dir(root.path().join(".hidden-project")).unwrap();
        let mut picker = picker();
        picker.open_add_project_at(root.path().to_path_buf());
        let area = Rect::new(0, 0, 120, 36);
        let layout = AddProjectLayout::new(area);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("visible"));
        assert!(rendered.contains("Show hidden"));
        assert!(!rendered.contains(".hidden-project"));

        assert_eq!(
            picker.handle_mouse_at(
                left_click(layout.hidden_toggle),
                area,
                std::time::Instant::now(),
            ),
            Intent::None
        );
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Hide hidden"));
        assert!(rendered.contains(".hidden-project"));
    }

    #[test]
    fn hidden_directory_visibility_survives_navigation_but_resets_for_a_new_dialog() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".hidden-project")).unwrap();
        fs::create_dir(root.path().join("visible")).unwrap();
        let root_path = fs::canonicalize(root.path()).unwrap();
        let visible_path = fs::canonicalize(root.path().join("visible")).unwrap();
        let mut picker = picker();
        picker.open_add_project_at(root.path().to_path_buf());

        assert_eq!(picker.handle_key(key(KeyCode::Char('.'))), Intent::None);
        for _ in 0..3 {
            picker.handle_key(key(KeyCode::Down));
        }
        assert_eq!(picker.handle_key(key(KeyCode::Enter)), Intent::None);
        assert!(matches!(
            &picker.view,
            View::AddProject(draft)
                if draft.current_dir == visible_path && draft.show_hidden
        ));

        assert_eq!(picker.handle_key(key(KeyCode::Backspace)), Intent::None);
        assert!(matches!(
            &picker.view,
            View::AddProject(draft)
                if draft.current_dir == root_path && draft.show_hidden
        ));

        picker.handle_key(key(KeyCode::Esc));
        picker.open_add_project_at(root.path().to_path_buf());
        assert!(matches!(
            &picker.view,
            View::AddProject(draft) if !draft.show_hidden
        ));
    }

    #[test]
    fn typing_in_add_project_filters_directories_and_escape_clears_the_filter() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("alpha-project")).unwrap();
        fs::create_dir(root.path().join("beta-project")).unwrap();
        let mut picker = picker();
        picker.open_add_project_at(root.path().to_path_buf());
        let area = Rect::new(0, 0, 120, 36);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();

        for character in "bet".chars() {
            assert_eq!(
                picker.handle_key(key(KeyCode::Char(character))),
                Intent::None
            );
        }
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("/  bet_"));
        assert!(rendered.contains("beta-project"));
        assert!(!rendered.contains("alpha-project"));

        assert_eq!(picker.handle_key(key(KeyCode::Backspace)), Intent::None);
        assert!(matches!(
            &picker.view,
            View::AddProject(draft) if draft.filter == "be"
        ));
        assert_eq!(picker.handle_key(key(KeyCode::Esc)), Intent::None);
        assert!(matches!(picker.view, View::AddProject(_)));
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("alpha-project"));
        assert!(rendered.contains("beta-project"));
    }

    #[test]
    fn typing_a_directory_filter_selects_the_first_match_for_enter() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("alpha-project")).unwrap();
        fs::create_dir(root.path().join("beta-project")).unwrap();
        let beta = fs::canonicalize(root.path().join("beta-project")).unwrap();
        let mut picker = picker();
        picker.open_add_project_at(root.path().to_path_buf());

        for character in "bet".chars() {
            picker.handle_key(key(KeyCode::Char(character)));
        }

        assert_eq!(picker.handle_key(key(KeyCode::Enter)), Intent::None);
        assert!(matches!(
            &picker.view,
            View::AddProject(draft) if draft.current_dir == beta
        ));
    }

    #[test]
    fn enter_does_nothing_when_a_directory_filter_has_no_matches() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("alpha-project")).unwrap();
        let root_path = fs::canonicalize(root.path()).unwrap();
        let mut picker = picker();
        picker.open_add_project_at(root.path().to_path_buf());
        for character in "missing".chars() {
            picker.handle_key(key(KeyCode::Char(character)));
        }

        assert_eq!(picker.handle_key(key(KeyCode::Enter)), Intent::None);
        assert!(matches!(
            &picker.view,
            View::AddProject(draft)
                if draft.current_dir == root_path && draft.filter == "missing"
        ));
    }

    #[test]
    fn filter_characters_and_arrow_navigation_do_not_conflict() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("jazz-one")).unwrap();
        fs::create_dir(root.path().join("jazz-two")).unwrap();
        let jazz_two = fs::canonicalize(root.path().join("jazz-two")).unwrap();
        let mut picker = picker();
        picker.open_add_project_at(root.path().to_path_buf());

        for character in "jAzZ".chars() {
            picker.handle_key(key(KeyCode::Char(character)));
        }
        assert!(matches!(
            &picker.view,
            View::AddProject(draft) if draft.filter == "jAzZ"
        ));
        assert_eq!(picker.handle_key(key(KeyCode::Esc)), Intent::None);
        assert!(matches!(picker.view, View::AddProject(_)));
        assert_eq!(picker.handle_key(key(KeyCode::Esc)), Intent::None);
        assert_eq!(picker.view, View::Browser);

        picker.open_add_project_at(root.path().to_path_buf());
        for character in "jazz".chars() {
            picker.handle_key(key(KeyCode::Char(character)));
        }
        picker.handle_key(key(KeyCode::Down));
        assert_eq!(picker.handle_key(key(KeyCode::Enter)), Intent::None);
        assert!(matches!(
            &picker.view,
            View::AddProject(draft) if draft.current_dir == jazz_two
        ));
    }

    #[test]
    fn mouse_can_enter_a_directory_and_add_it() {
        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("mouse-project");
        fs::create_dir(&project_path).unwrap();
        let project_path = fs::canonicalize(project_path).unwrap();
        let mut picker = picker();
        picker.open_add_project_at(root.path().to_path_buf());
        let area = Rect::new(0, 0, 120, 36);
        let layout = AddProjectLayout::new(area);
        let child_row = Rect::new(
            layout.directory_rows.x,
            layout.directory_rows.y + 2 * DIRECTORY_ROW_HEIGHT,
            layout.directory_rows.width,
            DIRECTORY_ROW_HEIGHT,
        );
        let first_click = std::time::Instant::now();

        assert_eq!(
            picker.handle_mouse_at(left_click(child_row), area, first_click),
            Intent::None
        );
        assert_eq!(
            picker.handle_mouse_at(
                left_click(child_row),
                area,
                first_click + Duration::from_millis(100),
            ),
            Intent::None
        );
        assert_eq!(
            picker.handle_mouse_at(
                left_click(layout.add_current),
                area,
                first_click + Duration::from_millis(200),
            ),
            Intent::AddProject(Project {
                id: "mouse-project".into(),
                name: "mouse-project".into(),
                path: project_path,
                agent: "pi".into(),
                base_branch: "main".into(),
                agent_args: Vec::new(),
            })
        );
    }

    #[test]
    fn mouse_uses_the_visible_directory_when_the_list_is_scrolled() {
        let root = tempfile::tempdir().unwrap();
        for name in ["alpha", "bravo", "charlie"] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        let expected = fs::canonicalize(root.path().join("bravo")).unwrap();
        let mut picker = picker();
        picker.open_add_project_at(root.path().to_path_buf());
        picker.directory_offset = 3;
        let area = Rect::new(0, 0, 120, 36);
        let layout = AddProjectLayout::new(area);
        let first_visible_row = layout.add_current;
        let first_click = std::time::Instant::now();

        assert_eq!(
            picker.handle_mouse_at(left_click(first_visible_row), area, first_click),
            Intent::None
        );
        assert_eq!(
            picker.handle_mouse_at(
                left_click(first_visible_row),
                area,
                first_click + Duration::from_millis(100),
            ),
            Intent::None
        );
        assert!(matches!(
            &picker.view,
            View::AddProject(draft) if draft.current_dir == expected
        ));
    }

    #[cfg(unix)]
    #[test]
    fn add_project_rejects_a_path_that_cannot_be_saved_as_utf8() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let mut picker = picker();
        let path = PathBuf::from(OsString::from_vec(vec![b'p', b'r', b'o', b'j', 0xff]));

        assert_eq!(picker.project_from_directory(&path), Intent::None);
        assert!(picker.error.as_deref().unwrap().contains("UTF-8"));
    }

    #[test]
    fn adding_a_non_git_directory_uses_the_configured_initial_branch() {
        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("new-project");
        fs::create_dir(&project_path).unwrap();
        let configured = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["config", "--get", "init.defaultBranch"])
            .output()
            .unwrap();
        let expected_branch = if configured.status.success() {
            String::from_utf8(configured.stdout)
                .unwrap()
                .trim()
                .to_owned()
        } else {
            DEFAULT_BASE_BRANCH.to_owned()
        };
        let project = Project {
            id: "new-project".into(),
            name: "new-project".into(),
            path: project_path.clone(),
            agent: DEFAULT_AGENT.into(),
            base_branch: DEFAULT_BASE_BRANCH.into(),
            agent_args: Vec::new(),
        };

        let normalized = normalize_project(project, &Config::default()).unwrap();

        assert_eq!(normalized.path, fs::canonicalize(&project_path).unwrap());
        let branch = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert!(branch.status.success());
        assert_eq!(
            String::from_utf8(branch.stdout).unwrap().trim(),
            expected_branch
        );
        assert_eq!(normalized.base_branch, expected_branch);
        let head = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["rev-parse", "--verify", "HEAD"])
            .output()
            .unwrap();
        assert!(head.status.success());
        let worktree = root.path().join("first-worktree");
        let created = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["worktree", "add", "-b", "first-worktree"])
            .arg(&worktree)
            .arg(&normalized.base_branch)
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
    }

    #[test]
    fn adding_an_existing_master_repository_detects_its_base_branch() {
        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("master-project");
        initialize_git_repository(&project_path, "master");
        let project = Project {
            id: "master-project".into(),
            name: "master-project".into(),
            path: project_path,
            agent: DEFAULT_AGENT.into(),
            base_branch: DEFAULT_BASE_BRANCH.into(),
            agent_args: Vec::new(),
        };

        let normalized = normalize_project(project, &Config::default()).unwrap();

        assert_eq!(normalized.base_branch, "master");
    }

    #[test]
    fn an_invalid_saved_base_branch_is_repaired_automatically() {
        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("saved-master-project");
        initialize_git_repository(&project_path, "master");
        let mut project = Project {
            id: "saved-master-project".into(),
            name: "saved-master-project".into(),
            path: project_path,
            agent: DEFAULT_AGENT.into(),
            base_branch: "main".into(),
            agent_args: Vec::new(),
        };

        assert!(repair_base_branch(&mut project).unwrap());
        assert_eq!(project.base_branch, "master");
    }

    #[test]
    fn dangling_main_is_repaired_to_an_existing_develop_branch() {
        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("dangling-main");
        initialize_git_repository(&project_path, "develop");
        let dangling = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["symbolic-ref", "HEAD", "refs/heads/main"])
            .output()
            .unwrap();
        assert!(dangling.status.success());
        let mut project = Project {
            id: "dangling-main".into(),
            name: "dangling-main".into(),
            path: project_path,
            agent: DEFAULT_AGENT.into(),
            base_branch: "missing".into(),
            agent_args: Vec::new(),
        };

        assert!(repair_base_branch(&mut project).unwrap());
        assert_eq!(project.base_branch, "develop");
    }

    #[test]
    fn detached_repository_uses_a_stable_commit_as_its_base() {
        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("detached");
        initialize_git_repository(&project_path, "main");
        let original = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(original.status.success());
        let original = String::from_utf8(original.stdout)
            .unwrap()
            .trim()
            .to_owned();
        let detached = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["checkout", "--detach"])
            .output()
            .unwrap();
        assert!(detached.status.success());
        let deleted = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["branch", "-D", "main"])
            .output()
            .unwrap();
        assert!(deleted.status.success());
        let project = Project {
            id: "detached".into(),
            name: "detached".into(),
            path: project_path.clone(),
            agent: DEFAULT_AGENT.into(),
            base_branch: DEFAULT_BASE_BRANCH.into(),
            agent_args: Vec::new(),
        };

        let mut normalized = normalize_project(project, &Config::default()).unwrap();

        assert_eq!(normalized.base_branch, original);
        let moved = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@localhost")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@localhost")
            .args(["commit", "--allow-empty", "--no-gpg-sign", "-m", "moved"])
            .output()
            .unwrap();
        assert!(moved.status.success());
        assert!(!repair_base_branch(&mut normalized).unwrap());
        assert_eq!(normalized.base_branch, original);
    }

    #[test]
    fn base_branch_repair_errors_are_returned_for_the_picker() {
        let mut config = Config {
            version: 1,
            ui: Default::default(),
            pins: Default::default(),
            projects: vec![Project {
                id: "missing".into(),
                name: "Missing".into(),
                path: PathBuf::from("/path/that/does/not/exist/switchyard"),
                agent: DEFAULT_AGENT.into(),
                base_branch: DEFAULT_BASE_BRANCH.into(),
                agent_args: Vec::new(),
            }],
        };

        let (changed, warning) = repair_config_base_branches(&mut config);

        assert!(!changed);
        assert!(warning.unwrap().contains("Missing"));
    }

    #[test]
    fn unborn_saved_base_branch_is_reported_as_unusable() {
        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("unborn-saved");
        fs::create_dir(&project_path).unwrap();
        let initialized = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .arg("init")
            .output()
            .unwrap();
        assert!(initialized.status.success());
        let branch = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["symbolic-ref", "--short", "HEAD"])
            .output()
            .unwrap();
        assert!(branch.status.success());
        let branch = String::from_utf8(branch.stdout).unwrap().trim().to_owned();
        let mut config = Config {
            version: 1,
            ui: Default::default(),
            pins: Default::default(),
            projects: vec![Project {
                id: "unborn-saved".into(),
                name: "Unborn saved".into(),
                path: project_path,
                agent: DEFAULT_AGENT.into(),
                base_branch: branch,
                agent_args: Vec::new(),
            }],
        };

        let (changed, warning) = repair_config_base_branches(&mut config);

        assert!(!changed);
        assert!(warning.unwrap().contains("does not resolve"));
    }

    #[test]
    fn remote_default_branch_wins_when_main_and_master_both_exist() {
        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("remote-default-project");
        initialize_git_repository(&project_path, "main");
        let master = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["branch", "master"])
            .output()
            .unwrap();
        assert!(master.status.success());
        let remote_master = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["update-ref", "refs/remotes/origin/master", "master"])
            .output()
            .unwrap();
        assert!(remote_master.status.success());
        let remote_head = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args([
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/master",
            ])
            .output()
            .unwrap();
        assert!(remote_head.status.success());
        let project = Project {
            id: "remote-default-project".into(),
            name: "remote-default-project".into(),
            path: project_path,
            agent: DEFAULT_AGENT.into(),
            base_branch: DEFAULT_BASE_BRANCH.into(),
            agent_args: Vec::new(),
        };

        let normalized = normalize_project(project, &Config::default()).unwrap();

        assert_eq!(normalized.base_branch, "master");
    }

    #[test]
    fn adding_a_repository_subdirectory_does_not_initialize_a_nested_repository() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let child = repository.join("child");
        fs::create_dir_all(&child).unwrap();
        let initialized = Command::new("git")
            .args(["-C"])
            .arg(&repository)
            .args(["init", "-b", "main"])
            .output()
            .unwrap();
        assert!(initialized.status.success());
        let project = Project {
            id: "child".into(),
            name: "child".into(),
            path: child.clone(),
            agent: DEFAULT_AGENT.into(),
            base_branch: DEFAULT_BASE_BRANCH.into(),
            agent_args: Vec::new(),
        };

        let error = normalize_project(project, &Config::default()).unwrap_err();

        assert!(error.to_string().contains("Git checkout root"));
        assert!(!child.join(".git").exists());
    }

    #[test]
    fn adding_a_bare_repository_reports_it_without_reinitializing() {
        let root = tempfile::tempdir().unwrap();
        let bare = root.path().join("bare.git");
        let initialized = Command::new("git")
            .args(["init", "--bare"])
            .arg(&bare)
            .output()
            .unwrap();
        assert!(initialized.status.success());
        let head_before = fs::read(bare.join("HEAD")).unwrap();
        let project = Project {
            id: "bare".into(),
            name: "bare".into(),
            path: bare.clone(),
            agent: DEFAULT_AGENT.into(),
            base_branch: DEFAULT_BASE_BRANCH.into(),
            agent_args: Vec::new(),
        };

        let error = normalize_project(project, &Config::default()).unwrap_err();

        assert!(error.to_string().contains("existing Git metadata"));
        assert_eq!(fs::read(bare.join("HEAD")).unwrap(), head_before);
    }

    #[test]
    fn adding_a_directory_with_broken_git_metadata_does_not_reinitialize_it() {
        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("broken");
        fs::create_dir(&project_path).unwrap();
        fs::write(
            project_path.join(".git"),
            "gitdir: /missing/switchyard-git-dir\n",
        )
        .unwrap();
        let project = Project {
            id: "broken".into(),
            name: "broken".into(),
            path: project_path.clone(),
            agent: DEFAULT_AGENT.into(),
            base_branch: DEFAULT_BASE_BRANCH.into(),
            agent_args: Vec::new(),
        };

        let error = normalize_project(project, &Config::default()).unwrap_err();

        assert!(error.to_string().contains("existing Git metadata"));
        assert_eq!(
            fs::read_to_string(project_path.join(".git")).unwrap(),
            "gitdir: /missing/switchyard-git-dir\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn adding_an_unborn_repository_finishes_bootstrap_without_running_hooks() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("unborn");
        fs::create_dir(&project_path).unwrap();
        let initialized = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .arg("init")
            .output()
            .unwrap();
        assert!(initialized.status.success());
        let initial_branch = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["symbolic-ref", "--short", "HEAD"])
            .output()
            .unwrap();
        assert!(initial_branch.status.success());
        let initial_branch = String::from_utf8(initial_branch.stdout)
            .unwrap()
            .trim()
            .to_owned();
        let hook = project_path.join(".git/hooks/pre-commit");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        let mut permissions = fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).unwrap();
        fs::write(project_path.join("staged.txt"), "keep staged\n").unwrap();
        let staged = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["add", "staged.txt"])
            .output()
            .unwrap();
        assert!(staged.status.success());
        let project = Project {
            id: "unborn".into(),
            name: "unborn".into(),
            path: project_path.clone(),
            agent: DEFAULT_AGENT.into(),
            base_branch: DEFAULT_BASE_BRANCH.into(),
            agent_args: Vec::new(),
        };

        let normalized = normalize_project(project, &Config::default()).unwrap();

        let branch = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(branch.stdout).unwrap().trim(),
            initial_branch
        );
        assert_eq!(normalized.base_branch, initial_branch);
        let status = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["status", "--short"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(status.stdout).unwrap().trim(),
            "A  staged.txt"
        );
    }

    #[test]
    fn adding_a_repository_with_dangling_head_does_not_rewrite_its_history() {
        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("damaged-head");
        fs::create_dir(&project_path).unwrap();
        let initialized = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .arg("init")
            .output()
            .unwrap();
        assert!(initialized.status.success());
        let committed = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@localhost")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@localhost")
            .args(["commit", "--allow-empty", "--no-gpg-sign", "-m", "existing"])
            .output()
            .unwrap();
        assert!(committed.status.success());
        let renamed = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["branch", "-m", "develop"])
            .output()
            .unwrap();
        assert!(renamed.status.success());
        let existing_commit = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["rev-parse", "develop"])
            .output()
            .unwrap()
            .stdout;
        let dangling = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["symbolic-ref", "HEAD", "refs/heads/missing"])
            .output()
            .unwrap();
        assert!(dangling.status.success());
        let head_before = fs::read(project_path.join(".git/HEAD")).unwrap();
        let project = Project {
            id: "damaged-head".into(),
            name: "damaged-head".into(),
            path: project_path.clone(),
            agent: DEFAULT_AGENT.into(),
            base_branch: DEFAULT_BASE_BRANCH.into(),
            agent_args: Vec::new(),
        };

        let error = normalize_project(project, &Config::default()).unwrap_err();

        assert!(error.to_string().contains("contains existing refs"));
        assert_eq!(
            fs::read(project_path.join(".git/HEAD")).unwrap(),
            head_before
        );
        let develop_after = Command::new("git")
            .args(["-C"])
            .arg(&project_path)
            .args(["rev-parse", "develop"])
            .output()
            .unwrap()
            .stdout;
        assert_eq!(develop_after, existing_commit);
        assert!(!project_path.join(".git/refs/heads/main").exists());
    }

    #[test]
    fn browser_draws_only_project_and_session_lists_without_global_bars() {
        let mut picker = picker_with_session();
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| picker.draw(frame)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Projects"));
        assert!(rendered.contains("Demo / Sessions"));
        assert!(rendered.contains("feat/one"));
        assert!(!rendered.contains("Switchyard"));
        assert!(!rendered.contains("↑/k"));
        assert!(!rendered.contains("Agent"));
        assert!(!rendered.contains("Mode"));
        assert!(!rendered.contains("Path"));
        assert!(!rendered.contains("Focus agent"));
    }

    #[test]
    fn focused_new_session_action_does_not_use_the_row_highlight_background() {
        let mut picker = picker();
        picker.handle_key(key(KeyCode::Enter));
        picker.handle_key(key(KeyCode::Char('/')));
        let area = Rect::new(0, 0, 120, 36);
        let layout = UiLayout::new(area);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| picker.draw(frame)).unwrap();

        let action_background = terminal
            .backend()
            .buffer()
            .cell((layout.new_session.x + 1, layout.new_session.y + 1))
            .unwrap()
            .bg;
        assert_eq!(action_background, picker.theme.panel);
        assert_ne!(action_background, picker.theme.selected_surface);
    }

    #[test]
    fn configured_themes_change_the_rendered_panel_and_accent_colors() {
        for (name, canvas, panel, accent, divider) in [
            (
                crate::model::ThemeName::JadeDark,
                Color::Rgb(11, 15, 20),
                Color::Rgb(17, 24, 33),
                Color::Rgb(104, 165, 141),
                Color::Rgb(41, 52, 61),
            ),
            (
                crate::model::ThemeName::MidnightDark,
                Color::Rgb(13, 16, 32),
                Color::Rgb(21, 26, 46),
                Color::Rgb(139, 156, 246),
                Color::Rgb(53, 59, 92),
            ),
            (
                crate::model::ThemeName::PaperLight,
                Color::Rgb(244, 247, 244),
                Color::Rgb(255, 255, 253),
                Color::Rgb(20, 122, 88),
                Color::Rgb(220, 228, 223),
            ),
            (
                crate::model::ThemeName::SandLight,
                Color::Rgb(239, 231, 216),
                Color::Rgb(250, 244, 232),
                Color::Rgb(139, 94, 52),
                Color::Rgb(216, 203, 182),
            ),
        ] {
            let mut config = picker().config;
            config.ui.theme = name;
            let mut picker = Picker::new(config, State::default(), RuntimeSnapshot::default());
            let backend = TestBackend::new(120, 36);
            let mut terminal = Terminal::new(backend).unwrap();

            terminal.draw(|frame| picker.draw(frame)).unwrap();

            let buffer = terminal.backend().buffer();
            assert_eq!(
                buffer.cell((0, 0)).unwrap().bg,
                canvas,
                "canvas for {name:?}"
            );
            let layout = UiLayout::new(Rect::new(0, 0, 120, 36));
            let panel_corner = buffer
                .cell((layout.projects_panel.x, layout.projects_panel.y))
                .unwrap();
            assert_eq!(panel_corner.bg, panel, "panel for {name:?}");
            assert_eq!(panel_corner.fg, accent, "accent for {name:?}");
            assert_eq!(
                buffer
                    .cell((layout.project_search.x, layout.project_search.bottom() - 1))
                    .unwrap()
                    .fg,
                divider,
                "divider for {name:?}"
            );
        }
    }

    #[test]
    fn mouse_click_selects_then_double_click_activates_a_session() {
        let mut picker = picker_with_session();
        picker.handle_key(key(KeyCode::Enter));
        let area = Rect::new(0, 0, 120, 36);
        let layout = UiLayout::new(area);
        let first_session_row = Rect::new(
            layout.session_rows.x,
            layout.session_rows.y,
            layout.session_rows.width,
            ROW_HEIGHT,
        );
        let click = left_click(first_session_row);
        let first_click = std::time::Instant::now();

        assert_eq!(
            picker.handle_mouse_at(click, area, first_click),
            Intent::None
        );
        assert_eq!(picker.session_selected, 1);
        assert_eq!(
            picker.handle_mouse_at(click, area, first_click + Duration::from_millis(100)),
            Intent::ActivateSession {
                project_id: "demo".into(),
                session_name: "feat/one".into(),
            }
        );
    }

    #[test]
    fn mouse_click_on_new_session_opens_the_form() {
        let mut picker = picker();
        let area = Rect::new(0, 0, 120, 36);
        let layout = UiLayout::new(area);

        assert_eq!(
            picker.handle_mouse_at(
                left_click(layout.new_session),
                area,
                std::time::Instant::now(),
            ),
            Intent::None
        );
        assert_eq!(
            picker.view,
            View::NewSession {
                project_id: "demo".into(),
                input: String::new(),
                mode: SessionMode::Worktree,
            }
        );
    }

    #[test]
    fn changing_projects_resets_the_session_scroll_position() {
        let mut picker = picker_with_many_sessions();
        picker.config.projects.push(Project {
            id: "other".into(),
            name: "Other".into(),
            path: PathBuf::from("/repos/other"),
            agent: "codex".into(),
            base_branch: "main".into(),
            agent_args: Vec::new(),
        });
        picker.state.sessions.push(Session {
            project_id: "other".into(),
            name: "only".into(),
            mode: SessionMode::Worktree,
            worktree_path: PathBuf::from("/worktrees/other/only"),
            pending_temporary_branch: None,
            created_at_ms: 20,
            last_used_at_ms: 20,
            agent_session: None,
            tab_id: None,
            tab_namespace: None,
        });
        picker.handle_key(key(KeyCode::Enter));
        for _ in 0..5 {
            picker.handle_key(key(KeyCode::Down));
        }
        picker.handle_key(key(KeyCode::Esc));
        picker.handle_key(key(KeyCode::Down));
        let area = Rect::new(0, 0, 120, 36);
        let layout = UiLayout::new(area);

        assert_eq!(
            picker.handle_mouse_at(
                MouseEvent {
                    kind: MouseEventKind::ScrollUp,
                    column: layout.sessions_panel.x + 1,
                    row: layout.sessions_panel.y + 1,
                    modifiers: KeyModifiers::NONE,
                },
                area,
                std::time::Instant::now(),
            ),
            Intent::None
        );
        assert_eq!(picker.session_selected, 1);
        assert_eq!(
            picker.displayed_session_name("other").as_deref(),
            Some("only")
        );
    }

    #[test]
    fn mouse_hit_testing_accounts_for_the_visible_table_offset() {
        let mut picker = picker_with_many_sessions();
        picker.handle_key(key(KeyCode::Enter));
        let area = Rect::new(0, 0, 80, 16);
        let layout = UiLayout::new(area);
        let visible_rows = usize::from(layout.session_rows.height / ROW_HEIGHT).max(1);
        for _ in 0..visible_rows + 2 {
            picker.handle_key(key(KeyCode::Down));
        }
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let visible_offset = picker.session_offset;
        let expected_session = {
            let sessions = picker.filtered_session_indices("demo");
            picker.state.sessions[sessions[visible_offset]].name.clone()
        };
        let first_visible_row = Rect::new(
            layout.session_rows.x,
            layout.session_rows.y,
            layout.session_rows.width,
            ROW_HEIGHT,
        );
        let click = left_click(first_visible_row);
        let first_click = std::time::Instant::now();

        assert_eq!(
            picker.handle_mouse_at(click, area, first_click),
            Intent::None
        );
        assert_eq!(
            picker.handle_mouse_at(click, area, first_click + Duration::from_millis(100)),
            Intent::ActivateSession {
                project_id: "demo".into(),
                session_name: expected_session,
            }
        );
    }

    #[test]
    fn mouse_ignores_the_partial_row_at_the_bottom_of_a_table() {
        let mut picker = picker_with_many_sessions();
        picker.handle_key(key(KeyCode::Enter));
        let (area, layout) = (10..50)
            .find_map(|height| {
                let area = Rect::new(0, 0, 120, height);
                let layout = UiLayout::new(area);
                (layout.session_rows.height >= 3 && layout.session_rows.height % ROW_HEIGHT == 1)
                    .then_some((area, layout))
            })
            .unwrap();
        let partial_row = Rect::new(
            layout.session_rows.x,
            layout.session_rows.bottom() - 1,
            layout.session_rows.width,
            1,
        );
        let click = left_click(partial_row);
        let first_click = std::time::Instant::now();

        assert_eq!(
            picker.handle_mouse_at(click, area, first_click),
            Intent::None
        );
        assert_eq!(picker.session_selected, 0);
        assert_eq!(
            picker.handle_mouse_at(click, area, first_click + Duration::from_millis(100)),
            Intent::None
        );
    }

    #[test]
    fn session_filter_does_not_filter_the_project_pane() {
        let mut picker = picker_with_session();
        picker.handle_key(key(KeyCode::Enter));
        picker.handle_key(key(KeyCode::Char('/')));
        for character in "feat".chars() {
            picker.handle_key(key(KeyCode::Char(character)));
        }

        assert_eq!(picker.filtered_project_indices(), vec![0]);
        assert_eq!(picker.filtered_session_indices("demo"), vec![0]);
    }

    #[test]
    fn an_unrelated_click_breaks_a_double_click_sequence() {
        let mut picker = picker_with_session();
        picker.handle_key(key(KeyCode::Enter));
        let area = Rect::new(0, 0, 120, 36);
        let layout = UiLayout::new(area);
        let session_row = Rect::new(
            layout.session_rows.x,
            layout.session_rows.y,
            layout.session_rows.width,
            ROW_HEIGHT,
        );
        let blank_border = Rect::new(layout.sessions_panel.x, layout.sessions_panel.y, 1, 1);
        let first_click = std::time::Instant::now();

        assert_eq!(
            picker.handle_mouse_at(left_click(session_row), area, first_click),
            Intent::None
        );
        assert_eq!(
            picker.handle_mouse_at(
                left_click(blank_border),
                area,
                first_click + Duration::from_millis(50),
            ),
            Intent::None
        );
        assert_eq!(
            picker.handle_mouse_at(
                left_click(session_row),
                area,
                first_click + Duration::from_millis(100),
            ),
            Intent::None
        );
    }

    #[derive(Default)]
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn failed_mouse_capture_io_keeps_cleanup_armed() {
        let mut restore = TerminalRestore::default();

        assert!(restore.enable_mouse_capture(&mut FailingWriter).is_err());
        assert!(restore.mouse_capture);
        assert!(restore.disable_mouse_capture(&mut FailingWriter).is_err());
        assert!(restore.mouse_capture);
        restore.mouse_capture = false;
    }
}
