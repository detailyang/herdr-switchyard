use std::env;

use anyhow::{Result, bail};
use herdr_switchyard::{herdr::CliHerdr, picker, store::Store};

fn main() -> Result<()> {
    let command = env::args().nth(1).unwrap_or_else(|| "picker".into());
    if matches!(command.as_str(), "help" | "--help" | "-h") {
        print_help();
        return Ok(());
    }
    if env::var_os("HERDR_ENV").as_deref() != Some(std::ffi::OsStr::new("1")) {
        bail!("Switchyard must run inside a Herdr plugin action or pane");
    }

    let store = Store::from_environment()?;
    let herdr = CliHerdr::from_environment();
    match command.as_str() {
        "open" => herdr.open_picker(),
        "picker" => picker::run(&store, &herdr),
        "sync" => picker::sync(&store, &herdr),
        "config-path" => {
            println!("{}", store.config_path().display());
            Ok(())
        }
        unknown => bail!("unknown command {unknown:?}; run with --help"),
    }
}

fn print_help() {
    println!(
        "Switchyard — project and worktree session switching for Herdr\n\n\
         Usage: herdr-switchyard <open|picker|sync|config-path>"
    );
}
