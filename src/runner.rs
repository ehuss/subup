use anyhow::{Context, Error};
use std::ffi::{OsStr, OsString};
use std::process::{Command, ExitStatus, Output, Stdio};

/// Helper for using `Command`.
#[must_use]
pub struct Runner {
    program: OsString,
    args: Vec<OsString>,
    cmd_str: String,
    dir: Option<String>,
    env: Vec<(OsString, OsString)>,
    wants_success: bool,
    inherit_stdout: bool,
}

impl Runner {
    pub fn new(program: impl AsRef<OsStr>, args: &[impl AsRef<OsStr>]) -> Runner {
        let vec_args: Vec<OsString> = args.iter().map(|s| s.as_ref().to_os_string()).collect();
        let cmd_str = {
            let mut vec_str: Vec<_> = vec_args.iter().map(|s| s.to_string_lossy()).collect();
            vec_str.insert(0, program.as_ref().to_string_lossy());
            vec_str.join(" ")
        };
        Runner {
            program: program.as_ref().to_os_string(),
            args: vec_args,
            cmd_str,
            dir: None,
            env: Vec::new(),
            wants_success: true,
            inherit_stdout: true,
        }
    }

    pub fn cmd_str(&self) -> &String {
        &self.cmd_str
    }

    pub fn args(mut self, args: &[impl AsRef<OsStr>]) -> Runner {
        self.args
            .extend(args.iter().map(|s| s.as_ref().to_os_string()));
        self
    }

    pub fn dir(mut self, dir: &str) -> Runner {
        self.dir = Some(dir.to_string());
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, val: impl AsRef<OsStr>) -> Runner {
        self.env
            .push((key.as_ref().to_os_string(), val.as_ref().to_os_string()));
        self
    }

    pub fn capture_stdout(&mut self, err_context: impl Into<String>) -> Result<String, Error> {
        self.inherit_stdout = false;
        let output = self.run(err_context)?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    pub fn status(&mut self, err_context: impl Into<String>) -> Result<ExitStatus, Error> {
        self.wants_success = false;
        self.inherit_stdout = false;
        let output = self.run(err_context)?;
        Ok(output.status)
    }

    pub fn run(&mut self, err_context: impl Into<String>) -> Result<Output, Error> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args).stderr(Stdio::inherit());
        if self.inherit_stdout {
            cmd.stdout(Stdio::inherit());
        } else {
            cmd.stdout(Stdio::piped());
        };
        if let Some(ref dir) = self.dir {
            if !dir.is_empty() {
                cmd.current_dir(dir);
            }
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        let output = cmd.output();
        match output {
            Ok(output) => {
                if output.status.code().is_none()
                    || (self.wants_success && output.status.code() != Some(0))
                    || (output.status.code() != Some(0) && output.status.code() != Some(1))
                {
                    Err(
                        anyhow::format_err!("Command exit status {:?}", output.status.code())
                            .context(format!("Failed to run command: {}", self.cmd_str))
                            .context(err_context.into()),
                    )
                } else {
                    Ok(output)
                }
            }
            Err(e) => Err(e)
                .with_context(|| format!("Failed to run command: {}", self.cmd_str))
                .with_context(|| err_context.into()),
        }
    }
}
