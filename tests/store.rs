use std::{fs, path::PathBuf};

use anyhow::{Result, anyhow};
use herdr_switchyard::{
    model::{Config, Project, Session, State, ThemeName, UiConfig},
    store::Store,
};
use tempfile::tempdir;

#[test]
fn missing_files_load_as_empty_versioned_documents() {
    let root = tempdir().unwrap();
    let store = Store::new(root.path().join("config"), root.path().join("state"));

    assert_eq!(store.load_config().unwrap(), Config::default());
    assert_eq!(store.load_state().unwrap(), State::default());
}

#[test]
fn project_config_and_session_state_round_trip_in_separate_files() {
    let root = tempdir().unwrap();
    let store = Store::new(root.path().join("config"), root.path().join("state"));
    let config = Config {
        version: 1,
        ui: Default::default(),
        projects: vec![Project {
            id: "demo".into(),
            name: "Demo".into(),
            path: PathBuf::from("/repos/demo"),
            agent: "codex".into(),
            base_branch: "main".into(),
            agent_args: Vec::new(),
        }],
    };

    store.save_config(&config).unwrap();
    store
        .update_state(|state| {
            state.sessions.push(Session {
                project_id: "demo".into(),
                name: "feat/one".into(),
                worktree_path: PathBuf::from("/worktrees/demo/feat-one"),
                pending_temporary_branch: None,
                created_at_ms: 1,
                last_used_at_ms: 2,
                agent_session: None,
            });
            Ok(())
        })
        .unwrap();

    assert_eq!(store.load_config().unwrap(), config);
    assert_eq!(store.load_state().unwrap().sessions[0].name, "feat/one");
    assert!(store.config_path().exists());
    assert!(store.state_path().exists());
}

#[test]
fn state_changes_survive_a_later_operation_error() {
    let root = tempdir().unwrap();
    let store = Store::new(root.path().join("config"), root.path().join("state"));

    let result: Result<()> = store.update_state(|state| {
        state.sessions.push(Session {
            project_id: "demo".into(),
            name: "feat/partial".into(),
            worktree_path: PathBuf::from("/worktrees/demo/feat-partial"),
            pending_temporary_branch: None,
            created_at_ms: 1,
            last_used_at_ms: 1,
            agent_session: None,
        });
        Err(anyhow!("agent failed after worktree creation"))
    });

    assert!(result.is_err());
    assert_eq!(store.load_state().unwrap().sessions[0].name, "feat/partial");
}

#[test]
fn legacy_session_branches_are_ignored() {
    let root = tempdir().unwrap();
    let state_dir = root.path().join("state");
    let state_path = state_dir.join("sessions.json");
    fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    fs::write(
        &state_path,
        r#"{"version":1,"sessions":[{"project_id":"demo","name":"Session 1","branch":"feat/one","worktree_path":"/worktrees/demo/session-1","created_at_ms":1,"last_used_at_ms":1}]}"#,
    )
    .unwrap();
    let store = Store::new(root.path().join("config"), state_dir);

    let state = store.load_state().unwrap();

    assert_eq!(state.sessions[0].name, "Session 1");
}

#[test]
fn unsupported_project_agents_are_rejected() {
    let root = tempdir().unwrap();
    let store = Store::new(root.path().join("config"), root.path().join("state"));
    let config = Config {
        version: 1,
        ui: Default::default(),
        projects: vec![Project {
            id: "demo".into(),
            name: "Demo".into(),
            path: PathBuf::from("/repos/demo"),
            agent: "codxe".into(),
            base_branch: "main".into(),
            agent_args: Vec::new(),
        }],
    };

    let error = store.save_config(&config).unwrap_err();

    assert!(error.to_string().contains("unsupported agent"));
}

#[test]
fn configs_without_ui_settings_use_the_existing_dark_theme() {
    let root = tempdir().unwrap();
    let config_dir = root.path().join("config");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("config.toml"), "version = 1\n").unwrap();
    let store = Store::new(config_dir, root.path().join("state"));

    let config = store.load_config().unwrap();

    assert_eq!(config.ui.theme, ThemeName::JadeDark);
}

#[test]
fn light_and_dark_theme_names_round_trip_through_plugin_config() {
    for theme in [
        ThemeName::JadeDark,
        ThemeName::MidnightDark,
        ThemeName::PaperLight,
        ThemeName::SandLight,
    ] {
        let root = tempdir().unwrap();
        let store = Store::new(root.path().join("config"), root.path().join("state"));
        let config = Config {
            version: 1,
            ui: UiConfig { theme },
            projects: Vec::new(),
        };

        store.save_config(&config).unwrap();

        assert_eq!(store.load_config().unwrap(), config);
    }
}

#[test]
fn unknown_theme_names_are_rejected() {
    let root = tempdir().unwrap();
    let config_dir = root.path().join("config");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        "version = 1\n\n[ui]\ntheme = \"neon-rainbow\"\n",
    )
    .unwrap();
    let store = Store::new(config_dir, root.path().join("state"));

    let error = store.load_config().unwrap_err();

    assert!(format!("{error:#}").contains("neon-rainbow"));
}
