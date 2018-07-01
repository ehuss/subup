use cli::Cli;
use failure::Error;
use regex::Regex;
use std::borrow::Cow;
use std::fs;
use std::iter;
use std::path::Path;

pub fn generate_commit(
    cli: &Cli,
    submodules: &[(&str, &str, String)],
    branch: &str,
) -> Result<(), Error> {
    let commit_re = Regex::new(r"(?m)^commit ([0-9A-Fa-f]+)").unwrap();
    let message_re = Regex::new(r"(?m)^$\n").unwrap();
    let summary_res = [
        Regex::new(
            r"(?mx)
        \s*Merge\ pull\ request\ \#(?P<pr>[0-9]+).*\n
        \s*(?P<summary>.*)",
        ).unwrap(),
        Regex::new(
            r"(?mx)
        \s*Auto\ merge\ of\ \#(?P<pr>[0-9]+).*\n
        \s*(?P<summary>.*)",
        ).unwrap(),
        Regex::new(r"(?mx)\s*(?P<summary>.*)").unwrap(),
    ];

    fn path_to_name(path: &str) -> Cow<str> {
        Path::new(path).file_name().unwrap().to_string_lossy()
    }

    let names = &submodules
        .iter()
        .map(|s| path_to_name(s.0))
        .collect::<Vec<_>>()
        .join(", ");
    let branch_alert = if branch == "master" {
        "".to_string()
    } else {
        format!("[{}] ", branch.to_uppercase())
    };
    let mut result = vec![format!("{}Update {}", branch_alert, names)];

    for (path, start_hash, end_hash) in submodules {
        // git log
        let output = cli.git(&format!("log --first-parent {}..{}", start_hash, end_hash))
            .dir(path)
            .capture_stdout("Failed to get log for submodule.")?;
        // Find where ^commit starts.
        let commit_starts: Vec<(usize, &str)> = commit_re
            .captures_iter(&output)
            .map(|c| {
                (
                    c.get(0).expect("missing match").start(),
                    c.get(1).expect("missing hash match").as_str(),
                )
            })
            .collect();
        // The end index of each commit message.
        let ends = commit_starts
            .iter()
            .skip(1)
            .map(|(start, _)| *start)
            .chain(iter::once(output.len()));
        // Collect each commit message and hash.
        let messages = commit_starts.iter().zip(ends).map(|((start, hash), end)| {
            let commit = &output[*start..end];
            // Skip past the headers.
            let message_start = message_re
                .find(commit)
                .expect("can't find commit message")
                .end();
            let message = &commit[message_start..];
            (hash, message)
        });
        // Extract a summary from the commit message.
        let summaries: Vec<(&str, &str, Option<&str>)> = messages
            .map(|(hash, message)| {
                let (summary, pr) = find_summary(&summary_res, message)?;
                Ok((*hash, summary, pr))
            })
            .collect::<Result<_, Error>>()?;
        // Create a commit summary.
        let mut submodule_summary = Vec::new();
        if submodules.len() > 1 {
            let name = path_to_name(path);
            submodule_summary.push(format!("## {}", name));
            submodule_summary.push("".to_string());
        }
        if summaries.len() > 12 {
            submodule_summary.push(format!(
                "{} commits in {}..{}",
                summaries.len(),
                start_hash,
                end_hash
            ));
            submodule_summary.push(format!(
                "{} to {}",
                git_date(cli, path, start_hash)?,
                git_date(cli, path, end_hash)?
            ));
        } else {
            for (_hash, summary, pr) in summaries {
                let extra = if let Some(pr) = pr {
                    let origin = git_origin(cli, path)?;
                    format!(" ({}#{})", origin, pr)
                } else {
                    String::new()
                };
                submodule_summary.push(format!("- {}{}", summary, extra));
            }
        }
        result.push(submodule_summary.join("\n"));
    }

    fs::write(".SUBUP_COMMIT_MSG", result.join("\n\n"))?;
    Ok(())
}

fn find_summary<'a>(
    summary_res: &[Regex],
    message: &'a str,
) -> Result<(&'a str, Option<&'a str>), Error> {
    for re in summary_res {
        if let Some(captures) = re.captures(message) {
            let summary = captures
                .name("summary")
                .expect("Can't find summary")
                .as_str();
            let pr = captures.name("pr").map(|m| m.as_str());
            return Ok((summary, pr));
        }
    }
    bail!("Could not find summary in {:?}", message);
}

fn git_date(cli: &Cli, path: &str, hash: &str) -> Result<String, Error> {
    Ok(cli.git(&format!("show -s --format=%ci {}", hash))
        .dir(path)
        .capture_stdout("Failed to get date for hash")?)
}

fn git_origin(cli: &Cli, path: &str) -> Result<String, Error> {
    let re = Regex::new(r"github.com[:/]([^/]+/[^.]+)\.git").unwrap();
    let origin = cli.git("config --get remote.origin.url")
        .dir(path)
        .capture_stdout("Failed to get origin")?;
    let c = re.captures(&origin)
        .ok_or_else(|| format_err!("Could not find github relative in `{}`", origin))?;
    Ok(c.get(1).unwrap().as_str().to_string())
}
