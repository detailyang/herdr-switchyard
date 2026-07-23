use std::{
    env, fs,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::de::DeserializeOwned;

use crate::model::{Config, State, is_supported_agent};

#[derive(Debug, Clone)]
pub struct Store {
    config_dir: PathBuf,
    state_dir: PathBuf,
}

impl Store {
    pub fn new(config_dir: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
            state_dir: state_dir.into(),
        }
    }

    pub fn from_environment() -> Result<Self> {
        let config_dir = env::var_os("HERDR_PLUGIN_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::config_dir().map(|path| path.join("herdr-switchyard")))
            .context("cannot resolve Switchyard config directory")?;
        let state_dir = env::var_os("HERDR_PLUGIN_STATE_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::state_dir().map(|path| path.join("herdr-switchyard")))
            .or_else(|| dirs::data_local_dir().map(|path| path.join("herdr-switchyard")))
            .context("cannot resolve Switchyard state directory")?;
        Ok(Self::new(config_dir, state_dir))
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn state_path(&self) -> PathBuf {
        self.state_dir.join("sessions.json")
    }

    pub fn load_config(&self) -> Result<Config> {
        load_document(&self.config_path(), |input| {
            toml::from_str(input).context("parse TOML")
        })
        .and_then(validate_config_version)
    }

    pub fn save_config(&self, config: &Config) -> Result<()> {
        validate_config_version(config.clone())?;
        let output = toml::to_string_pretty(config).context("serialize project configuration")?;
        atomic_write(&self.config_path(), output.as_bytes())
    }

    pub fn load_state(&self) -> Result<State> {
        self.load_state_unlocked()
    }

    pub fn update_state<T>(&self, update: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        fs::create_dir_all(&self.state_dir)
            .with_context(|| format!("create {}", self.state_dir.display()))?;
        let lock_path = self.state_dir.join("sessions.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        lock.lock_exclusive()
            .with_context(|| format!("lock {}", lock_path.display()))?;

        let mut state = self.load_state_unlocked()?;
        let update_result = update(&mut state);
        let save_result = self.save_state_unlocked(&state);
        let unlock_result =
            FileExt::unlock(&lock).with_context(|| format!("unlock {}", lock_path.display()));
        save_result?;
        unlock_result?;
        update_result
    }

    fn load_state_unlocked(&self) -> Result<State> {
        load_document(&self.state_path(), |input| {
            serde_json::from_str(input).context("parse JSON")
        })
        .and_then(validate_state_version)
    }

    fn save_state_unlocked(&self, state: &State) -> Result<()> {
        validate_state_version(state.clone())?;
        let output = serde_json::to_vec_pretty(state).context("serialize session registry")?;
        atomic_write(&self.state_path(), &output)
    }
}

fn load_document<T: Default + DeserializeOwned>(
    path: &Path,
    parse: impl FnOnce(&str) -> Result<T>,
) -> Result<T> {
    match fs::read_to_string(path) {
        Ok(input) => parse(&input).with_context(|| format!("read {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn validate_config_version(config: Config) -> Result<Config> {
    if config.version != 1 {
        bail!("unsupported Switchyard config version {}", config.version);
    }
    for project in &config.projects {
        if !is_supported_agent(&project.agent) {
            bail!(
                "unsupported agent {:?} for project {:?}; expected codex, claude, or pi",
                project.agent,
                project.name
            );
        }
    }
    Ok(config)
}

fn validate_state_version(state: State) -> Result<State> {
    if state.version != 1 {
        bail!("unsupported Switchyard state version {}", state.version);
    }
    Ok(state)
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("state path has no UTF-8 file name")?;
    let temporary = parent.join(format!(".{file_name}.tmp.{}", std::process::id()));
    let mut file =
        File::create(&temporary).with_context(|| format!("create {}", temporary.display()))?;
    file.write_all(contents)
        .with_context(|| format!("write {}", temporary.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}
