// Copyright (C) 2026 by GiGa infosystems

//! See the documentation of [`Command`], a utility type for running commands (in this case `git`,
//! `cargo` and `rustc`).

use anyhow::{Error, Result, anyhow, bail};
use std::ffi::{OsStr, OsString};
use std::fmt::Write;
use std::iter;
use std::path::{Path, PathBuf};
use std::process::{self, ExitStatus};
use std::time::Duration;

#[must_use]
#[derive(Clone)]
pub struct CommandBuilder<'a, 'b> {
    sandbox: Option<&'b rustwide::Build<'a>>,
    path: Option<&'b Path>,
    timeout: Option<Duration>,
}

impl<'a, 'b> CommandBuilder<'a, 'b> {
    pub const fn new() -> Self {
        CommandBuilder {
            sandbox: None,
            path: None,
            timeout: None,
        }
    }

    pub const fn in_sandbox_maybe(self, sandbox: Option<&'b rustwide::Build<'a>>) -> Self {
        CommandBuilder { sandbox, ..self }
    }

    pub const fn at_path_maybe(self, path: Option<&'b Path>) -> Self {
        CommandBuilder { path, ..self }
    }

    pub const fn with_timeout_maybe(self, timeout: Option<Duration>) -> Self {
        CommandBuilder { timeout, ..self }
    }

    pub fn cmd<const N: usize>(self, cmd: [&str; N]) -> Command<'a> {
        const {
            if N == 0 {
                panic!("The command is empty");
            }
        };
        let backend = match self.sandbox {
            None => {
                let mut cmd = std::process::Command::new(cmd[0]);
                if let Some(path) = self.path {
                    cmd.current_dir(path);
                }
                CommandBackend::Normal(cmd)
            }
            Some(sandbox) => {
                let mut cmd = if cmd[0] == "cargo" {
                    sandbox.cargo()
                } else {
                    sandbox.cmd(cmd[0])
                };
                if let Some(path) = self.path {
                    cmd = cmd.current_directory(path);
                }
                CommandBackend::Sandboxed(cmd)
            }
        };

        let mut out = Command {
            name: cmd[0].to_owned(),
            backend,
            timeout: self.timeout,
        };
        for arg in &cmd[1..] {
            write!(out.name, " {arg}").expect("this shouldn't fail");
            out = out.arg(arg);
        }
        out
    }
}

enum CommandBackend<'a> {
    Normal(std::process::Command),
    Sandboxed(rustwide::cmd::Command<'a, 'static>),
}

/// Used to run commands in a uniform manner regardless of sandboxed status, geared towards the
/// use-cases of this crate (such as convenient accessing of stdout as a string with the final
/// newline stripped, see [`Command::stdout`]).
#[must_use]
pub struct Command<'a> {
    name: String,
    timeout: Option<Duration>,
    backend: CommandBackend<'a>,
}

impl<'a> Command<'a> {
    pub const fn builder<'b>() -> CommandBuilder<'a, 'b> {
        CommandBuilder::new()
    }

    /// Run an external command, to set more options see [`Command::builder`].
    pub fn cmd<const N: usize>(cmd: [&str; N]) -> Self {
        Self::builder().cmd(cmd)
    }

    pub fn arg(mut self, arg: impl ValidArg) -> Self {
        for i in arg.as_args() {
            match self.backend {
                CommandBackend::Normal(ref mut cmd) => {
                    cmd.arg(i);
                }
                CommandBackend::Sandboxed(cmd) => {
                    self.backend = CommandBackend::Sandboxed(cmd.arg(i))
                }
            };
        }
        self
    }

    fn run_inner(self, capture_stdout: bool, assert_success: bool) -> Result<process::Output> {
        let name = self.name;
        let timeout_error = || {
            anyhow!(
                "Failed to run `{name}`, took longer than {:?}",
                self.timeout
            )
        };
        let assert_success = |status: ExitStatus| {
            if assert_success && !status.success() {
                bail!("Failed to run `{name}`, returned status code {status}");
            } else {
                Ok(())
            }
        };
        match self.backend {
            CommandBackend::Normal(mut cmd) => {
                cmd.stderr(std::io::stderr());
                if !capture_stdout {
                    // Push all command output to stderr as well
                    cmd.stdout(std::io::stderr());
                }

                let output = if let Some(timeout) = self.timeout {
                    // `std` sadly doesn't have a nice way of creating timeouts, and `rustwide` uses
                    // `tokio` anyways:
                    tokio::runtime::LocalRuntime::new()?.block_on(async {
                        tokio::time::timeout(timeout, tokio::process::Command::from(cmd).output())
                            .await
                            .map_err(|_| timeout_error())
                            .and_then(|result| result.map_err(Error::from))
                    })?
                } else {
                    cmd.output()?
                };

                assert_success(output.status)?;

                Ok(output)
            }
            CommandBackend::Sandboxed(cmd) => {
                let cmd = cmd
                    .log_command(false)
                    .log_output(false)
                    .timeout(self.timeout);

                let output = if capture_stdout {
                    cmd.run_capture()
                } else {
                    // We sadly don't know if it's stdout or stderr :(
                    cmd.process_lines(&mut |line, _| eprintln!("{line}"))
                        .run_capture()
                };

                match output {
                    Ok(output) => {
                        if capture_stdout {
                            for line in output.stderr_lines() {
                                eprintln!("{line}");
                            }
                        }

                        Ok(process::Output {
                            status: ExitStatus::default(),
                            stdout: output.stdout_lines().join("\n").into(),
                            stderr: Vec::new(),
                        })
                    }
                    Err(rustwide::cmd::CommandError::ExecutionFailed { status, stderr }) => {
                        // We didn't emit stderr in `process_lines` earlier, stdout is sadly now
                        // lost
                        if capture_stdout {
                            eprintln!("{stderr}");
                        }

                        assert_success(status)?;
                        Ok(process::Output {
                            status,
                            stdout: Vec::new(), // sadly not recoverable
                            stderr: stderr.into(),
                        })
                    }
                    // In these two cases, if stdout was captured, we sadly don't know stderr
                    // anymore:
                    Err(rustwide::cmd::CommandError::Timeout(_)) => Err(timeout_error()),
                    Err(other) => Err(other.into()),
                }
            }
        }
    }

    /// Run the command, failing on non-zero exit statuses, and not capturing stdout
    pub fn run(self) -> Result<()> {
        self.run_inner(false, true)?;
        Ok(())
    }

    /// Run the command, not capturing stdout, returning `true` if the exit status was zero
    #[expect(clippy::wrong_self_convention)]
    pub fn is_success(self) -> Result<bool> {
        let out = self.run_inner(false, false)?.status.success();
        Ok(out)
    }

    /// Run the command, failing on non-zero exit statuses, and capturing stdout and returning it,
    /// with the final newline removed if there was one (ie for the output of
    /// `rustc --print host-tuple`)
    pub fn stdout(self) -> Result<String> {
        let mut out = self.run_inner(true, true)?.stdout;
        if out.last() == Some(&b'\n') {
            out.pop();
        }
        Ok(String::from_utf8(out)?)
    }
}

pub trait ValidArg {
    fn as_args(&self) -> impl Iterator<Item = &OsStr>;
}

impl<T: ValidArg + ?Sized> ValidArg for &'_ T {
    fn as_args(&self) -> impl Iterator<Item = &OsStr> {
        T::as_args(self)
    }
}

impl ValidArg for OsStr {
    fn as_args(&self) -> impl Iterator<Item = &OsStr> {
        iter::once(self)
    }
}

impl ValidArg for OsString {
    fn as_args(&self) -> impl Iterator<Item = &OsStr> {
        OsStr::as_args(&**self)
    }
}

impl ValidArg for str {
    fn as_args(&self) -> impl Iterator<Item = &OsStr> {
        iter::once(self.as_ref())
    }
}

impl ValidArg for String {
    fn as_args(&self) -> impl Iterator<Item = &OsStr> {
        str::as_args(self)
    }
}

impl ValidArg for Path {
    fn as_args(&self) -> impl Iterator<Item = &OsStr> {
        iter::once(self.as_ref())
    }
}

impl ValidArg for PathBuf {
    fn as_args(&self) -> impl Iterator<Item = &OsStr> {
        Path::as_args(self)
    }
}

impl<T: ValidArg> ValidArg for Option<T> {
    fn as_args(&self) -> impl Iterator<Item = &OsStr> {
        self.as_ref().into_iter().flat_map(|inner| inner.as_args())
    }
}

impl<T: ValidArg> ValidArg for [T] {
    fn as_args(&self) -> impl Iterator<Item = &OsStr> {
        self.iter().flat_map(|inner| inner.as_args())
    }
}

impl<T: ValidArg, const N: usize> ValidArg for [T; N] {
    fn as_args(&self) -> impl Iterator<Item = &OsStr> {
        <[T]>::as_args(self)
    }
}

impl<T: ValidArg> ValidArg for Vec<T> {
    fn as_args(&self) -> impl Iterator<Item = &OsStr> {
        <[T]>::as_args(self)
    }
}

/// This makes flags with arguments much nicer
impl<T: ValidArg> ValidArg for (&str, T) {
    fn as_args(&self) -> impl Iterator<Item = &OsStr> {
        self.0.as_args().chain(self.1.as_args())
    }
}
