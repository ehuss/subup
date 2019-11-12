use crate::cli::Cli;
use anyhow::{bail, format_err, Error};
use lazy_static::lazy_static;
use regex::Regex;
use std::borrow::Cow;
use std::fs;
use std::iter;
use std::path::Path;

pub fn generate_commit(
    cli: &Cli,
    // (path, start_hash, end_hash)
    submodules: &[(&str, impl AsRef<str>, impl AsRef<str>)],
    branch: &str,
) -> Result<(), Error> {
    let commit_re = Regex::new(r"(?m)^commit ([0-9A-Fa-f]+)").unwrap();
    let message_re = Regex::new(r"(?m)^$\n").unwrap();
    let summary_res = [
        Regex::new(
            r"(?mx)
        \s*Merge\ pull\ request\ \#(?P<pr>[0-9]+).*\n
        \s*(?P<summary>.*)",
        )
        .unwrap(),
        Regex::new(
            r"(?mx)
        \s*Auto\ merge\ of\ \#(?P<pr>[0-9]+).*\n
        \s*(?P<summary>.*)",
        )
        .unwrap(),
        Regex::new(r"(?mx)\s*(?P<summary>.*)").unwrap(),
    ];
    let gh_short_re = Regex::new(r"(?:^|\B)(#[0-9]+)\b").unwrap();

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
        let start_hash = start_hash.as_ref();
        let end_hash = end_hash.as_ref();
        let origin = git_origin(cli, path)?;
        // git log
        let output = cli
            .git(&format!("log --first-parent {}..{}", start_hash, end_hash))
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
            let headers = &commit[..message_start];
            let message = &commit[message_start..];
            (hash, headers, message)
        });
        // Extract a summary from the commit message.
        let mut summaries = Vec::new();
        for (hash, headers, message) in messages {
            for (summary, pr) in find_summary(&summary_res, headers, message)? {
                // Rewrite github relative links to the correct path.
                let summary = summary.replace("<", "&lt;").replace(">", "&gt;");
                let summary = gh_short_re
                    .replace_all(&summary, format!("{}$1", origin).as_str())
                    .into_owned();
                summaries.push((*hash, summary, pr));
            }
        }
        // Create a commit summary.
        let mut submodule_summary = Vec::new();
        if submodules.len() > 1 {
            let name = path_to_name(path);
            submodule_summary.push(format!("## {}", name));
            submodule_summary.push("".to_string());
        }
        // if summaries.len() > 15 {
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
        // } else {
        for (_hash, summary, pr) in summaries {
            let extra = if let Some(pr) = pr {
                format!(" ({}#{})", origin, pr)
            } else {
                String::new()
            };
            submodule_summary.push(format!("- {}{}", summary, extra));
        }
        // }
        result.push(submodule_summary.join("\n"));
    }

    fs::write(".SUBUP_COMMIT_MSG", result.join("\n\n"))?;
    Ok(())
}

fn find_summary<'a>(
    summary_res: &[Regex],
    headers: &'a str,
    message: &'a str,
) -> Result<Vec<(&'a str, Option<&'a str>)>, Error> {
    if headers.contains("bors[bot]") && message.contains("Merge #") {
        // bors-ng style consolidated merge
        lazy_static! {
            static ref NG_RE: Regex = Regex::new(r"(?m)^\s*([0-9]+): (.*)(?:r=.* a=.*$)").unwrap();
        }
        let results: Vec<_> = NG_RE
            .captures_iter(message)
            .map(|cap| {
                (
                    cap.get(2).unwrap().as_str(),
                    Some(cap.get(1).unwrap().as_str()),
                )
            })
            .collect();
        if !results.is_empty() {
            return Ok(results);
        }
    }
    for re in summary_res {
        if let Some(captures) = re.captures(message) {
            let summary = captures
                .name("summary")
                .expect("Can't find summary")
                .as_str();
            let pr = captures.name("pr").map(|m| m.as_str());
            return Ok(vec![(summary, pr)]);
        }
    }
    bail!("Could not find summary in {:?}", message);
}

fn git_date(cli: &Cli, path: &str, hash: &str) -> Result<String, Error> {
    Ok(cli
        .git(&format!("show -s --format=%ci {}", hash))
        .dir(path)
        .capture_stdout("Failed to get date for hash")?)
}

fn git_origin(cli: &Cli, path: &str) -> Result<String, Error> {
    let re = Regex::new(r"github.com[:/]([^/]+/[^.]+)(\.git)?").unwrap();
    let origin = cli
        .git("config --get remote.origin.url")
        .dir(path)
        .capture_stdout("Failed to get origin")?;
    let c = re
        .captures(&origin)
        .ok_or_else(|| format_err!("Could not find github relative in `{}`", origin))?;
    Ok(c.get(1).unwrap().as_str().to_string())
}
