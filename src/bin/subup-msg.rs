#![warn(rust_2018_idioms)]

use anyhow::Error;
use clap::{App, Arg};

use subup::cli::Cli;
use subup::log;

fn get_hash(cli: &Cli<'_>, tree: &str, path: &str) -> Result<String, Error> {
    let output = cli
        .git(&format!("ls-tree upstream/{} {}", tree, path))
        .capture_stdout("Failed to ls-tree")?;
    Ok(output
        .split_whitespace()
        .skip(2)
        .next()
        .unwrap()
        .to_string())
}

fn current_hash(cli: &Cli<'_>, path: &str) -> Result<String, Error> {
    let output = cli
        .git("rev-parse --verify HEAD")
        .dir(path)
        .capture_stdout(format!(
            "Failed to determine rev `HEAD` for path `{}`",
            path
        ))?;
    Ok(output)
}

fn doit(cli: &Cli<'_>) -> Result<(), Error> {
    cli.status("Generating .SUBUP_COMMIT_MSG")?;
    // (path, start_hash, end_hash)
    let submodules = cli
        .matches
        .values_of("submodules")
        .unwrap()
        .map(|submodule| {
            let first = get_hash(cli, cli.matches.value_of("branch").unwrap(), submodule)?;
            let current = current_hash(cli, submodule)?;
            Ok((submodule, first, current))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    log::generate_commit(cli, &submodules)?;
    cli.status("Complete")?;
    Ok(())
}

fn main() {
    let matches = App::new("subup-msg")
        .version(clap::crate_version!())
        .about("Generate commit message")
        .setting(clap::AppSettings::ColoredHelp)
        .arg(
            Arg::with_name("submodules")
                .help("Submodules to examine")
                .multiple(true)
                .required(true),
        )
        .arg(
            Arg::with_name("verbose")
                .long("verbose")
                .short("v")
                .help("Verbose output"),
        )
        .arg(
            Arg::with_name("branch")
                .long("branch")
                .help("Parent branch")
                .default_value("master"),
        )
        .get_matches();

    let cli = Cli::new(matches);
    cli.doit(doit);
}
