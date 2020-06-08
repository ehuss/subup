#![warn(rust_2018_idioms)]

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};
use std::process::exit;

use anyhow::{bail, format_err, Context, Error};
use cargo_metadata::{Metadata, Package, PackageId};
use clap::{App, Arg};

use subup::cli::Cli;

use subup::log;

/// Cargo workspace member.
#[derive(Debug, Hash, Eq, PartialEq)]
struct Member {
    name: String,
    _version: String,
    /// Absolute path to a workspace member.
    path: PathBuf,
}

/// Git submodule.
#[derive(Debug, Hash, Eq, PartialEq)]
struct Submodule {
    /// Relative path to the submodule.
    path: String,
    /// The branch or revision it should update to.
    rev: String,
    /// True if the submodule was listed on command line.
    wants_update: bool,
    /// This is set to `true` if the submodule was updated and new changes
    /// were found.
    was_updated: bool,
    /// The original git hash for the submodule before updating.
    original_hash: String,
    /// Cargo workspace members found within this submodule.
    members: Vec<Member>,
}

struct SubUp<'a> {
    cli: &'a Cli<'a>,
    /// All submodules in the repo.
    submodules: Vec<Submodule>,
    /// The target branch of the rust repo (master/beta/stable)
    rust_branch: String,
    /// The branch name to create for the pull request.
    up_branch: String,
    /// Metadata of workspace before updating submodules.
    /// None until after base branch is updated.
    orig_metadata: Option<Metadata>,
}

impl<'a> SubUp<'a> {
    fn submodules_to_up(&self) -> impl Iterator<Item = &Submodule> {
        self.submodules.iter().filter(|s| s.wants_update)
    }

    fn updated_submodules(&self) -> impl Iterator<Item = &Submodule> {
        self.submodules.iter().filter(|s| s.was_updated)
    }

    fn has_changes(&self, path: &str) -> Result<bool, Error> {
        // TODO: Some references online use the following to check for changes:
        //     git diff-files --quiet
        //     git diff-index --cached --quiet
        // However, a plain `diff-index --quiet` seems to work for both
        // unstaged and staged changes, so I'm not sure if there's a reason to
        // run both commands.
        Ok(!self
            .cli
            .git(&format!("diff-index --quiet HEAD {}", path))
            .status("Failed to check for changes.")?
            .success())
    }

    fn update_submodules_base(&self) -> Result<(), Error> {
        self.cli.status("Updating submodules to base.")?;
        // TODO: Add --progress?
        self.cli
            .git("submodule update --init --recursive")
            .run("Failed to init/update submodules.")?;
        Ok(())
    }

    fn check_environment(&mut self) -> Result<(), Error> {
        self.cli.status("Checking working tree.")?;
        if !Path::new(".git").exists() {
            bail!(".git not found, are you in the root directory?");
        }
        // Make sure index is set up (otherwise diff-index compares against
        // all zero hashes).
        self.cli
            .git("update-index -q --refresh")
            .run("Failed to update-index.")?;

        // Check for changes.
        if self.has_changes(".")? {
            // TODO: This will probably have problems. It would be nice to
            // support it.
            self.cli.warning("Working tree has changes.")?;
            self.cli
                .git("status --porcelain")
                .run("Failed to get git status.")?;
            if !self.cli.matches.is_present("allow-changes")
                && !self.cli.confirm("Do you want to continue?", true)?
            {
                self.cli.exit_err();
            }
        }

        // Check rust_branch.
        if self.rust_branch == "master" {
            self.cli.info("Using branch `master`")?;
        } else {
            self.cli
                .warning(&format!("Using non-master branch `{}`", self.rust_branch))?;
        }

        // Check upstream.
        let has_upstream = self
            .cli
            .git("config remote.upstream.url")
            .status("Failed to get upstream url.")?
            .success();
        if !has_upstream {
            if self.cli.is_interactive() {
                self.cli.warning("`upstream` is not configured.")?;
                let upstream = self
                    .cli
                    .input(
                        "Please enter the upstream url",
                        Some("https://github.com/rust-lang/rust.git"),
                    )?
                    .unwrap();
                self.cli
                    .git(&format!("remote add upstream {}", upstream))
                    .run("Failed to add upstream.")?;
            } else {
                bail!("`upstream` remote is not configured.");
            }
        }
        Ok(())
    }

    fn get_hash(&self, rev: &str, path: &str) -> Result<String, Error> {
        let output = self
            .cli
            .git(&format!("rev-parse --verify {}", rev))
            .dir(path)
            .capture_stdout(format!(
                "Failed to determine rev `{}` for path `{}`",
                rev, path
            ))?;
        Ok(output)
    }

    fn check_args(&mut self) -> Result<(), Error> {
        self.cli.status("Checking module names.")?;
        // Get information about every submodule, and the Cargo workspace
        // members it has.
        let output = self
            .cli
            .git("config --file .gitmodules --get-regexp path")
            .capture_stdout("Failed to get submodule list.")?;
        let paths = output.lines().map(|line| {
            let parts: Vec<_> = line.split(' ').collect();
            assert_eq!(parts.len(), 2);
            parts[1]
        });
        for path in paths {
            let members = SubUp::compute_members(self.orig_metadata.as_ref().unwrap(), path)?;
            let original_hash = self.get_hash(&format!("HEAD:{}", path), ".")?;
            let submodule = Submodule {
                path: path.to_string(),
                rev: "master".to_string(), // Will set below.
                wants_update: false,       // Will set below.
                was_updated: false,
                original_hash,
                members,
            };
            self.submodules.push(submodule);
        }
        // Check user arguments.
        for arg in self.cli.matches.values_of("submodules").unwrap() {
            let parts: Vec<_> = arg.splitn(2, ':').collect();
            let (path, rev) = if parts.len() == 1 {
                if self.rust_branch != "master" {
                    self.cli.warning(&format!(
                        "Did not specify a branch for module `{}`.",
                        parts[0]
                    ))?;
                    let rev = self.cli.input(
                        &format!("Which branch or revision should `{}` use?", parts[0]),
                        None,
                    )?;
                    if rev.is_none() {
                        bail!("You must specify a branch or rev for module `{}`", parts[0]);
                    }
                    (parts[0].to_string(), rev.unwrap())
                } else {
                    // TODO: This probably shouldn't assume master, but the
                    // value in .gitmodules may not be accurate.
                    (parts[0].to_string(), "master".to_string())
                }
            } else {
                (parts[1].to_string(), parts[0].to_string())
            };
            let submodule = self
                .submodules
                .iter_mut()
                .find(|submodule| submodule.path == path)
                .ok_or_else(|| {
                    format_err!("Could not find submodule `{}` in git modules.", path)
                })?;
            submodule.rev = rev;
            submodule.wants_update = true;
        }
        Ok(())
    }

    fn fetch_submodules(&self) -> Result<(), Error> {
        self.cli.status("Fetching submodules.")?;
        // TODO: This may not be necessary after `submodule update`?
        for submodule in self.submodules_to_up() {
            self.cli
                .git("fetch --tags")
                .dir(&submodule.path)
                .run(format!("Failed to fetch in module `{}`.", submodule.path))?;
        }
        Ok(())
    }

    fn check_submodule_rev(&mut self) -> Result<(), Error> {
        self.cli.status("Checking submodule revs.")?;
        let mut to_change = HashMap::new();
        for submodule in self.submodules_to_up() {
            // Verify the rev name is correct.
            let origin = format!("origin/{}", submodule.rev);
            if self.get_hash(&origin, &submodule.path).is_ok() {
                to_change.insert(submodule.path.clone(), origin);
            } else {
                self.get_hash(&submodule.rev, &submodule.path)?;
            }
        }
        for (path, rev) in to_change {
            let submodule = self
                .submodules
                .iter_mut()
                .find(|submodule| &submodule.path == &path)
                .ok_or_else(|| {
                    format_err!("Could not find submodule `{}` in git modules.", path)
                })?;
            submodule.rev = rev;
        }
        Ok(())
    }

    fn check_for_updates(&self) -> Result<(), Error> {
        // Check if any of the submodules were actually modified.
        let mut found = false;
        for submodule in self.submodules_to_up() {
            let was_modified = !self
                .cli
                .git(&format!("diff-index --quiet {}", submodule.rev))
                .dir(&submodule.path)
                .status("Failed to check for changes.")?
                .success();
            if was_modified {
                found = true;
                break;
            }
        }
        if !found {
            self.cli
                .warning("Submodules do not have any changes, exiting...")?;
            exit(0);
        }
        Ok(())
    }

    fn make_branch(&mut self) -> Result<(), Error> {
        self.cli.status("Fetching upstream.")?;
        self.cli
            .git("fetch upstream")
            .run("Failed to fetch upstream.")?;

        self.cli.status("Creating branch.")?;
        self.cli
            .git(&format!(
                "checkout -B {} upstream/{}",
                &self.up_branch, self.rust_branch
            ))
            .run("Failed to create branch.")?;

        // TODO: Is there a better way to do this?
        self.cli
            .git(&format!("config branch.{}.remote origin", self.up_branch))
            .run("Failed to configure remote.")?;
        self.cli
            .git(&format!(
                "config branch.{}.merge refs/heads/{}",
                self.up_branch, self.up_branch
            ))
            .run("Failed to configure head.")?;

        self.update_submodules_base()?;
        Ok(())
    }

    fn check_branch(&mut self) -> Result<(), Error> {
        self.cli.status("Checking head branch.")?;
        // Check if the branch already exists.
        let branch_exists = self
            .cli
            .git(&format!(
                "show-ref --verify --quiet refs/heads/{}",
                self.up_branch
            ))
            .status("Failed to check branch status.")?
            .success();
        if branch_exists {
            self.cli.warning(&format!(
                "Branch `{}` already exists.  It will be reset.",
                self.up_branch
            ))?;
            // TODO: cli option to allow.
            if !self.cli.confirm("Do you want to continue?", true)? {
                self.cli.exit_err();
            }
        }
        Ok(())
    }

    fn update_submodules(&self) -> Result<(), Error> {
        self.cli.status("Updating submodules.")?;
        for submodule in self.submodules_to_up() {
            self.cli
                .git(&format!("checkout {}", &submodule.rev))
                .dir(&submodule.path)
                .run(format!(
                    "Failed to checkout rev `{}` in module `{}`.",
                    submodule.rev, submodule.path
                ))?;
        }
        Ok(())
    }

    fn check_submodule_updated(&mut self) -> Result<(), Error> {
        self.cli.status("Checking for updated submodules.")?;

        let new_metadata = load_metadata()?;
        let mods_updated: Vec<bool> = self
            .submodules_to_up()
            .map(|m| self.has_changes(&m.path))
            .collect::<Result<Vec<bool>, Error>>()?;
        for (submodule, updated) in &mut self
            .submodules
            .iter_mut()
            .filter(|s| s.wants_update)
            .zip(mods_updated)
        {
            submodule.was_updated = updated;
            // In case the members changes in this update, recompute.
            let members = SubUp::compute_members(&new_metadata, &submodule.path)?;
            submodule.members = members;
        }

        for submodule in self.submodules_to_up() {
            if !submodule.was_updated {
                self.cli.warning(&format!(
                    "Module `{}` did not have any changes.",
                    submodule.path
                ))?;
            }
        }
        if !self.submodules_to_up().any(|m| m.was_updated) {
            self.cli.warning("No submodules were updated, exiting...")?;
            exit(0);
        }
        Ok(())
    }

    /// Determine which members are in a submodule.
    fn compute_members(metadata: &Metadata, submodule_path: &str) -> Result<Vec<Member>, Error> {
        let mut members = Vec::new();
        let package_map: HashMap<&PackageId, &Package> = metadata
            .packages
            .iter()
            .map(|package| (&package.id, package))
            .collect();
        let abs_path = env::current_dir()?.join(&submodule_path);
        for member in &metadata.workspace_members {
            let package = package_map[member];
            // Pop `Cargo.toml` off path.
            let member_path = package.manifest_path.parent().unwrap();
            if member_path.strip_prefix(&abs_path).is_ok() {
                members.push(Member {
                    name: package.name.clone(),
                    _version: package.version.to_string(),
                    path: member_path.to_path_buf(),
                });
            }
        }
        Ok(members)
    }

    fn update_lock_submodule(&self, member: &Member) -> Result<(), Error> {
        // TODO: Use version?  Would need to use version from new metadata.
        // TODO: Support windows?
        self.cli
            .cargo(&format!("update -p file://{}", member.path.display()))
            .dir("src")
            .run(format!(
                "Failed to update Cargo.lock for pkg `{}`.",
                member.name
            ))?;
        Ok(())
    }

    fn update_lock(&self) -> Result<(), Error> {
        self.cli.status("Updating Cargo.lock")?;
        for submodule in self.updated_submodules() {
            // TODO: This does not support adding a new member.
            for member in &submodule.members {
                // Check if Cargo.toml was updated.
                let was_updated = !self
                    .cli
                    .git(&format!(
                        "diff-index --quiet {} Cargo.toml",
                        submodule.original_hash
                    ))
                    .dir(member.path.to_str().unwrap())
                    .status("Failed to determine if Cargo.toml changed.")?
                    .success();
                if was_updated {
                    self.update_lock_submodule(member)?;
                } else {
                    if self.cli.matches.is_present("verbose") {
                        self.cli.info(&format!(
                            "Skipping member `{}`, manifest was not changed.",
                            member.name
                        ))?;
                    }
                }
            }
        }
        if self.has_changes("Cargo.lock")? {
            self.cli.warning("Cargo.lock has changed.")?;
            if !self.cli.is_interactive() && !self.cli.matches.is_present("allow-lock-change") {
                bail!("Cargo.lock changes requires --allow-lock-change, aborting...");
            }
            if self.cli.is_interactive() {
                self.cli
                    .info("Please carefully inspect Cargo.lock changes.")?;
                if !self.cli.confirm("Do you want to continue?", true)? {
                    bail!("Aborting...");
                }
            }
        }
        Ok(())
    }

    fn test(&self) -> Result<(), Error> {
        // TODO: Remove submodules that can't be tested?
        let mut default = HashSet::new();
        let cli_test = self
            .cli
            .matches
            .values_of("test")
            .map(|tests| {
                tests
                    .flat_map(|s| s.split_whitespace().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_else(|| vec!["default".to_string()]);
        for choice in cli_test {
            if choice == "skip" {
                self.cli.warning("`skip` specified, tests skipped.")?;
                return Ok(());
            } else if choice == "default" {
                for submodule in self.updated_submodules() {
                    default.insert(submodule.path.clone());
                }
            } else {
                default.insert(choice.to_string());
            }
        }
        let default: Vec<String> = default.into_iter().collect();
        // This behavior is a little weird, consider changing.
        let mut to_test = if self.cli.is_interactive() {
            let default = default.join(" ");
            let input = self
                .cli
                .input("Enter the submodules to test", Some(&default))?
                .unwrap();
            if input == "" {
                Vec::new()
            } else {
                input.split(' ').map(|s| s.to_string()).collect()
            }
        } else {
            if !self.cli.matches.is_present("test") {
                self.cli.warning("Tests skipped, use --test to test.")?;
                return Ok(());
            }
            default
        };
        // TODO: better way to skip
        if to_test.is_empty() || to_test == ["skip"] {
            self.cli.warning("Skipping tests.")?;
        } else {
            // Prevent bootstrap from changing the submodules.
            self.cli
                .runner("./configure", &["--disable-manage-submodules"])
                .run("Failed to disable submodules in config.toml.")?;
            self.cli
                .status(&format!("Running tests for {}", to_test.join(" ")))?;
            to_test.insert(0, "test".to_string());
            self.cli
                .runner("./x.py", &to_test)
                .run("Failed to run `x.py test`")?;
        }
        Ok(())
    }

    fn git_add(&self) -> Result<(), Error> {
        self.cli.status("Adding to git index.")?;
        let mut to_add: Vec<_> = self
            .updated_submodules()
            .map(|submodule| submodule.path.clone())
            .collect();
        to_add.push("Cargo.lock".to_string());
        self.cli
            .git("add")
            .args(&to_add)
            .run("Failed to add files to git.")?;
        Ok(())
    }

    fn prepare_commit_message(&self) -> Result<(), Error> {
        self.cli.status("Preparing commit message.")?;
        let ups: Vec<_> = self
            .updated_submodules()
            .map(|submodule| {
                let new_hash = self.get_hash(&format!(":{}", &submodule.path), ".")?;
                Ok((
                    submodule.path.as_str(),
                    submodule.original_hash.as_str(),
                    new_hash,
                ))
            })
            .collect::<Result<_, Error>>()?;
        log::generate_commit(self.cli, &ups)?;
        Ok(())
    }

    fn finish(&self) -> Result<(), Error> {
        println!("Please review changes.");
        println!("If satisfied, run:");
        println!("git commit -F .SUBUP_COMMIT_MSG");
        println!("git push -f");
        Ok(())
    }

    fn run(&mut self) -> Result<(), Error> {
        self.check_environment()?;
        self.check_branch()?;
        self.make_branch()?;
        self.orig_metadata = Some(load_metadata()?);
        self.check_args()?;
        self.fetch_submodules()?;
        self.check_submodule_rev()?;
        self.check_for_updates()?;
        self.update_submodules()?;
        self.check_submodule_updated()?;
        self.update_lock()?;
        self.git_add()?;
        self.prepare_commit_message()?;
        self.test()?;
        self.finish()?;
        Ok(())
    }
}

/// Determine the head branch name to use.
fn up_branch(cli: &Cli<'_>, rust_branch: &str) -> String {
    if let Some(branch) = cli.matches.value_of("up-branch") {
        branch.to_string()
    } else {
        // Compute the branch name.
        let mut parts = Vec::new();
        parts.push(Cow::from("update"));
        if rust_branch != "master" {
            parts.push(Cow::from(rust_branch));
        }
        parts.extend(
            cli.matches
                .values_of("submodules")
                .unwrap()
                .map(|m| Path::new(m).file_name().unwrap().to_string_lossy()),
        );
        parts.join("-")
    }
}

/// Determine the base branch name to use (master/beta/stable).
fn rust_branch(cli: &Cli<'_>) -> Result<String, Error> {
    if let Some(branch) = cli.matches.value_of("rust-branch") {
        Ok(branch.to_string())
    } else {
        let branch = cli
            .git("symbolic-ref --short HEAD")
            .capture_stdout("Could not determine current branch.")?;
        if !["master", "beta", "stable"].iter().any(|b| *b == branch) {
            cli.warning(&format!(
                "Current branch `{}` is not master/beta/stable.",
                branch
            ))?;
            let branch = cli.input("Which base branch do you want to use?", Some("master"))?;
            if let Some(branch) = branch {
                return Ok(branch);
            } else {
                cli.warning("Use `--rust-branch` to explicitly specify the base branch.")?;
                exit(1);
            }
        }
        Ok(branch)
    }
}

fn load_metadata() -> Result<Metadata, Error> {
    // TODO: Temp hack to deal with clippy needing nightly due to edition feature.
    env::set_var("RUSTC_BOOTSTRAP", "1");
    let m = cargo_metadata::MetadataCommand::new()
        .no_deps()
        .exec()
        .context("Failed to load cargo metadata.")?;
    Ok(m)
}

fn doit(cli: &Cli<'_>) -> Result<(), Error> {
    let rust_branch = rust_branch(cli)?;
    let up_branch = up_branch(cli, &rust_branch);

    let mut s = SubUp {
        cli,
        submodules: Vec::new(),
        rust_branch,
        up_branch,
        orig_metadata: None,
    };
    s.run()
}

fn main() {
    let matches = App::new("subup")
        .version(clap::crate_version!())
        .about("Update rust repo submodules")
        .setting(clap::AppSettings::ColoredHelp)
        .arg(
            Arg::with_name("submodules")
                .help(
                    "Submodules to update (src/tools/cargo, etc.), \
                     prefix with `branchname:` to specify the branch to use",
                )
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
            Arg::with_name("allow-changes")
                .long("allow-changes")
                .help("Allow command to run with existing git changes"),
        )
        .arg(
            Arg::with_name("rust-branch")
                .long("rust-branch")
                .takes_value(true)
                .help("The target rust branch (master/beta/stable)"),
        )
        .arg(
            Arg::with_name("up-branch")
                .long("up-branch")
                .takes_value(true)
                .help("The branch name to create (defaults to update-{module})"),
        )
        .arg(
            Arg::with_name("allow-lock-change")
                .long("allow-lock-change")
                .help("Allow updating Cargo.lock in non-interactive mode."),
        )
        .arg(
            Arg::with_name("test")
                .long("test")
                .takes_value(true)
                .multiple(true)
                .use_delimiter(true)
                .help("Always run the given tests on modified submodules."),
        )
        .get_matches();

    let cli = Cli::new(matches);
    cli.doit(doit);
}
