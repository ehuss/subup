use std::ffi::OsStr;
use std::io::Write;
use std::process::exit;

use crate::runner::Runner;
use anyhow::Error;
use clap::ArgMatches;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Select};
use termcolor::{BufferWriter, Color, ColorChoice, ColorSpec, WriteColor};

pub struct Cli<'a> {
    pub matches: ArgMatches<'a>,
    out_writer: BufferWriter,
    theme: ColorfulTheme,
    is_interactive: bool,
}

impl<'a> Cli<'a> {
    pub fn new(matches: ArgMatches<'a>) -> Cli {
        let is_interactive = !matches.is_present("force") && atty::is(atty::Stream::Stdout);
        Cli {
            matches,
            out_writer: BufferWriter::stdout(ColorChoice::Auto),
            theme: ColorfulTheme::default(),
            is_interactive,
        }
    }

    pub fn doit(&self, f: impl Fn(&Cli) -> Result<(), Error>) -> ! {
        if let Err(e) = f(&self) {
            let _ = self.emit_message("Error: ", Color::Red, &e.to_string(), true);
            for cause in e.chain().skip(1) {
                let _ = self.emit_message("Caused by: ", Color::Red, &cause.to_string(), true);
            }
            self.exit_err();
        }
        exit(0)
    }

    pub fn exit_err(&self) -> ! {
        exit(1)
    }

    fn emit_message(
        &self,
        header: &str,
        color: Color,
        message: &str,
        bold: bool,
    ) -> Result<(), Error> {
        let mut buffer = self.out_writer.buffer();
        buffer.set_color(ColorSpec::new().set_fg(Some(color)).set_bold(true))?;
        buffer.write_all(header.as_bytes())?;
        buffer.reset()?;
        if bold {
            buffer.set_color(ColorSpec::new().set_bold(true))?;
        }
        buffer.write_all(message.as_bytes())?;
        buffer.reset()?;
        buffer.write_all(b"\n")?;
        self.out_writer.print(&buffer)?;
        Ok(())
    }

    pub fn warning(&self, message: &str) -> Result<(), Error> {
        self.emit_message("Warning: ", Color::Yellow, message, true)
    }

    pub fn status(&self, message: &str) -> Result<(), Error> {
        self.emit_message("Status: ", Color::Green, message, true)
    }

    pub fn info(&self, message: &str) -> Result<(), Error> {
        self.emit_message("Info: ", Color::Blue, message, false)
    }

    pub fn is_interactive(&self) -> bool {
        self.is_interactive
    }

    pub fn confirm(&self, message: &str, default: bool) -> Result<bool, Error> {
        if self.matches.is_present("force") {
            return Ok(default);
        }
        if !self.is_interactive() {
            return Ok(false);
        }
        Ok(Confirm::with_theme(&self.theme)
            .with_prompt(message)
            .default(default)
            .interact()?)
    }

    pub fn input(&self, message: &str, default: Option<&str>) -> Result<Option<String>, Error> {
        if self.matches.is_present("force") {
            return Ok(default.map(|s| s.to_string()));
        }
        if !self.is_interactive() {
            return Ok(None);
        }
        let mut input: Input<'_, String> = Input::with_theme(&self.theme);
        input.with_prompt(message);
        if let Some(d) = default {
            input.default(d.to_string());
        }
        Ok(Some(input.interact()?))
    }

    pub fn select(
        &self,
        prompt: &str,
        items: &[&str],
        default: Option<usize>,
    ) -> Result<Option<usize>, Error> {
        if self.matches.is_present("force") {
            return Ok(default);
        }
        if !self.is_interactive() {
            return Ok(None);
        }
        let mut select = Select::with_theme(&self.theme);
        select.with_prompt(prompt).items(items);
        if let Some(default) = default {
            select.default(default);
        }
        Ok(select.interact_opt()?)
    }

    /// Create a `Runner` (a wrapper around `Command`).
    pub fn runner(&self, program: impl AsRef<OsStr>, args: &[impl AsRef<OsStr>]) -> Runner {
        let r = Runner::new(program, args);
        if self.matches.is_present("verbose") {
            let _ = self.info(&format!("Running: {}", r.cmd_str()));
        }
        r
    }

    pub fn git(&self, args: &str) -> Runner {
        let args: Vec<_> = args.split_whitespace().collect();
        self.runner("git", &args)
    }

    pub fn cargo(&self, args: &str) -> Runner {
        let args: Vec<_> = args.split_whitespace().collect();
        // TODO: This should use the version of cargo from stage0,
        // but I'm uncertain how to get the path.
        let runner = self.runner("cargo", &args);
        // Hack because clippy currently requires nightly, and this will
        // override the nightly feature check.
        runner.env("RUSTC_BOOTSTRAP", "1")
    }
}
