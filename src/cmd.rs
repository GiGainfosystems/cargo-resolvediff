// Copyright (C) 2026 by GiGa infosystems

//! See the documentation of [`cmd!`], a utility macro for running commands (in this case `git`,
//! `cargo` and `rustc`).

/// Run an external process
///
/// # Usage
/// Example: `cmd!([cargo "generate-lockfile"] ["--manifest-path" (path)])`, where all segments
/// (`cargo`, `"generate-lockfile"`, `"--manifest-path"` and `(path)`) can be either identifiers,
/// which get stringified, literals or expressions in parentheses. The first set of arguments is
/// also used in error reporting ("Failed to run `cargo generate-lockfile`").
///
/// If there are no further arguments, the second set of brackets is omitted.
///
/// Additionally, it may output a boolean (where the returned status code is either `0` mapped to
/// `true` or `1` mapped to `false`) by adding `-> bool`, or alternatively the stdout output
/// excluding a single trailing newline if it exists by adding `-> String`.
///
/// It may also be run in another working directory using `in path` (after potential return
/// specifiers as explained above), where `path` is an expression of the type
/// `Option<impl AsRef<Path>>`, or a reference to such a type.
macro_rules! cmd {
    (@arg $ident:ident) => { stringify!($ident) };
    (@arg $literal:literal) => { $literal };
    (@arg ($expr:expr)) => { $expr };
    (@stdout $cmd:ident -> String) => { std::process::Stdio::piped() };
    (@stdout $cmd:ident $(-> $ty:ident)?) => { std::io::stderr() };
    (@success $out:ident -> bool) => { true };
    (@success $out:ident $(-> $ty:ident)?) => { $out.status.success() };
    (@out $out:ident -> bool) => { $out.status.success() };
    (@out $out:ident -> String) => {{
        let mut out = $out.stdout;

        if out.last() == Some(&b'\n') {
            out.pop();
        }

        String::from_utf8(out)?
    }};
    (@out $out:ident) => { () };
    ([$cmd0:tt $($cmd_args:tt)*] $([$($args:tt)*])? $(-> $ret:tt)? $(in $path:expr)?) => {{
        let cmd0 = $crate::cmd::cmd!(@arg $cmd0);
        let cmd_args: [&str;_] = [$($crate::cmd::cmd!(@arg $cmd_args)),*];
        let mut cmd = std::process::Command::new(cmd0);
        cmd.args(&cmd_args)
            $($(.arg($crate::cmd::cmd!(@arg $args)))?)?;

        $(
            if let Some(path) = $path {
                cmd.current_dir(path);
            }
        )?

        cmd.stdout($crate::cmd::cmd!(@stdout cmd $(-> $ret)?));

        let output = cmd.spawn()?.wait_with_output()?;

        if !$crate::cmd::cmd!(@success output $(-> $ret)?) {
            color_eyre::eyre::bail!(
                "Failed to run `{} {}`, returned status code {}",
                cmd0,
                cmd_args.join(" "),
                output.status,
            );
        }

        <color_eyre::Result<_>>::Ok($crate::cmd::cmd!(@out output $(-> $ret)?))
    }};
}

pub(crate) use cmd;
