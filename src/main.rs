// Copyright (C) 2026 by GiGa infosystems

// NOTE: This doesn't handle `git` dependencies currently, as they cannot really be detected in
// `cargo metadata` outside of parsing the source.
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use clap::Parser;
use crates_io_api::SyncClient;
use semver::Version;
use serde::Serialize;

use cargo_resolvediff::Platform;
use cargo_resolvediff::diff::Diff;
use cargo_resolvediff::git::Repository;
use cargo_resolvediff::major_updates::{
    LatestVersion, ManifestDependencySet, fetch_latest_major_update_for,
};
use cargo_resolvediff::resolve::{Resolved, SpecificCrateIdent};
use cargo_resolvediff::util::{
    check, detect_toolchain, fetch, host_platform, locate_project, update,
};

const TEMPLATING_ENV_PREFIX: &str = "resolvediff_env_";
const TEMPLATING_ENV_VAR_PREFIX: &str = "env_";

struct OutputConfig {
    templated_output: bool,
    /// only set if `templated_output` is also `true`
    templated_major_as_squashed: bool,
    templated_in_json: bool,
    jinja: minijinja::Environment<'static>,
}

impl OutputConfig {
    const MINOR_COMMIT: &str = "minor_commit.jinja";
    const MINOR_OUTPUT: &str = "minor_output.jinja";
    const MAJOR_COMMIT: &str = "major_commit.jinja";
    const MAJOR_OUTPUT: &str = "major_output.jinja";
    const SQUASHED_COMMIT: &str = "squashed_commit.jinja";
    const SQUASHED_OUTPUT: &str = "squashed_output.jinja";
    const GIT_OUTPUT: &str = "git_output.jinja";

    const DEFAULT_TEMPLATES: &[(&str, &str)] = &[
        (
            "_default_templates_body.jinja",
            include_str!("default_templates/_default_templates_body.jinja"),
        ),
        (
            "_default_templates_helpers.jinja",
            include_str!("default_templates/_default_templates_helpers.jinja"),
        ),
        (
            Self::MINOR_COMMIT,
            include_str!("default_templates/minor_commit.jinja"),
        ),
        (
            Self::MINOR_OUTPUT,
            include_str!("default_templates/minor_output.jinja"),
        ),
        (
            Self::MAJOR_COMMIT,
            include_str!("default_templates/major_commit.jinja"),
        ),
        (
            Self::MAJOR_OUTPUT,
            include_str!("default_templates/major_output.jinja"),
        ),
        (
            Self::SQUASHED_COMMIT,
            include_str!("default_templates/squashed_commit.jinja"),
        ),
        (
            Self::SQUASHED_OUTPUT,
            include_str!("default_templates/squashed_output.jinja"),
        ),
        (
            Self::GIT_OUTPUT,
            include_str!("default_templates/git_output.jinja"),
        ),
    ];

    const WAS_TEMPLATED_ERR: &str = "Was templated, and as such is always a string";

    fn init_jinja(
        platforms: &[Platform],
        path: Option<&Path>,
    ) -> Result<minijinja::Environment<'static>> {
        let mut jinja = minijinja::Environment::new();

        let short_platform = {
            let mapping = platforms
                .iter()
                .map(|platform| {
                    let short = if let Some((short, _)) = platform.0.rsplit_once('-')
                        && !platforms
                            .iter()
                            .any(|other| platform != other && other.0.starts_with(short))
                    {
                        short
                    } else {
                        &platform.0
                    };
                    (platform.0.clone(), short.replace("-unknown", ""))
                })
                .collect::<HashMap<_, _>>();
            move |platform: String| mapping[&platform].clone()
        };

        jinja.add_filter("short_platform", short_platform);

        if let Some(path) = path {
            if !path.is_dir() {
                bail!("Template directory doesn't exist");
            }

            jinja.set_loader(minijinja::path_loader(&path));
        }

        for (name, template) in Self::DEFAULT_TEMPLATES {
            if let Some(path) = path
                && path.join(name).is_file()
            {
                // Template exists
                jinja.get_template(name)?;
                continue;
            }

            jinja.add_template(name, template)?;
        }

        for (name, value) in env::vars_os() {
            let Ok(mut name) = name.into_string() else {
                continue;
            };

            if !name
                .get(..TEMPLATING_ENV_PREFIX.len())
                .is_some_and(|s| s.eq_ignore_ascii_case(TEMPLATING_ENV_PREFIX))
            {
                continue;
            }

            let Ok(value) = value.into_string() else {
                eprintln!("Warning: The environment variable {name:?} isn't valid unicode");
                continue;
            };

            name.replace_range(..TEMPLATING_ENV_PREFIX.len(), TEMPLATING_ENV_VAR_PREFIX);

            jinja.add_global(name, value);
        }

        Ok(jinja)
    }

    fn output(
        &self,
        name: &str,
        ctx: minijinja::Value,
        commit: Option<&str>,
    ) -> Result<serde_json::Value> {
        let mut ctx = minijinja::context! {
            commit => commit,
            ..ctx
        };

        if self.templated_in_json {
            let templated = self.jinja.get_template(name)?.render(&ctx)?;
            ctx = minijinja::context! {
                templated => templated,
                ..ctx
            };
        }

        if self.templated_output {
            Ok(self.jinja.get_template(name)?.render(&ctx)?.into())
        } else {
            Ok(serde_json::to_value(&ctx)?)
        }
    }

    fn minor_commit(&self, diff: &Diff<'_>) -> Result<String> {
        Ok(self.jinja.get_template(Self::MINOR_COMMIT)?.render(diff)?)
    }

    fn minor_output(&self, diff: &Diff<'_>, commit: Option<&str>) -> Result<serde_json::Value> {
        self.output(
            Self::MINOR_OUTPUT,
            minijinja::Value::from_serialize(diff),
            commit,
        )
    }

    fn major_context(diff: &Diff<'_>, package: &str, version: &Version) -> minijinja::Value {
        minijinja::context! {
            package => package,
            version => version,
            ..minijinja::Value::from_serialize(diff),
        }
    }

    fn major_commit(&self, diff: &Diff<'_>, package: &str, version: &Version) -> Result<String> {
        let out = self
            .jinja
            .get_template(Self::MAJOR_COMMIT)?
            .render(Self::major_context(diff, package, version))?;
        Ok(out)
    }

    fn major_output(
        &self,
        diff: &Diff<'_>,
        package: &str,
        version: &Version,
        commit: Option<&str>,
    ) -> Result<serde_json::Value> {
        self.output(
            Self::MAJOR_OUTPUT,
            Self::major_context(diff, package, version),
            commit,
        )
    }

    fn squashed_context(
        diff: &Diff<'_>,
        major_updates: &[SpecificCrateIdent],
        failed_major_updates: &[SpecificCrateIdent],
    ) -> minijinja::Value {
        minijinja::context! {
            major_updates => major_updates,
            failed_major_updates => failed_major_updates,
            ..minijinja::Value::from_serialize(diff),
        }
    }

    fn squashed_commit(
        &self,
        diff: &Diff<'_>,
        major_updates: &[SpecificCrateIdent],
        failed_major_updates: &[SpecificCrateIdent],
    ) -> Result<String> {
        let out =
            self.jinja
                .get_template(Self::SQUASHED_COMMIT)?
                .render(Self::squashed_context(
                    diff,
                    major_updates,
                    failed_major_updates,
                ))?;
        Ok(out)
    }

    fn squashed_output(
        &self,
        diff: &Diff<'_>,
        major_updates: &[SpecificCrateIdent],
        failed_major_updates: &[SpecificCrateIdent],
        commit: Option<&str>,
    ) -> Result<serde_json::Value> {
        self.output(
            Self::SQUASHED_OUTPUT,
            Self::squashed_context(diff, major_updates, failed_major_updates),
            commit,
        )
    }

    fn git_output(&self, diff: &Diff<'_>, from: &str, to: &str) -> Result<serde_json::Value> {
        self.output(
            Self::GIT_OUTPUT,
            minijinja::context! {
                from => from,
                to => to,
                ..minijinja::Value::from_serialize(diff),
            },
            Some(to),
        )
    }

    fn merged_major_output(
        &self,
        squashed_diff: &Diff<'_>,
        updates: &MajorUpdates,
    ) -> Result<serde_json::Value> {
        if self.templated_major_as_squashed {
            self.squashed_output(
                squashed_diff,
                &updates.major_order,
                &updates.failed_major_updates,
                None,
            )
        } else if self.templated_output {
            let mut out = updates
                .minor
                .as_str()
                .expect(Self::WAS_TEMPLATED_ERR)
                .to_owned();
            for i in &updates.major_order {
                while !out.ends_with("\n\n") {
                    out.push('\n');
                }

                let update = &updates.major_updates[&i.name];
                out.push_str(update.as_str().expect(Self::WAS_TEMPLATED_ERR));
            }
            Ok(out.into())
        } else {
            Ok(serde_json::to_value(updates)?)
        }
    }

    fn final_output(&self, value: &serde_json::Value) -> Result<()> {
        if self.templated_output {
            println!("{}", value.as_str().expect(Self::WAS_TEMPLATED_ERR));
        } else {
            output_json(value)?;
        }

        Ok(())
    }
}

fn output_json(value: &impl Serialize) -> Result<()> {
    use std::io::{self, IsTerminal};

    if io::stdout().is_terminal() {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", serde_json::to_string(value)?);
    }

    Ok(())
}

/// This program does both minor updates (using `cargo update`) and major updates (by editing the
/// `Cargo.toml`s in the workspace), and produces review diffs between each step for the dependency
/// resolution for the given platforms.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// The path to the manifest of the workspace to update
    ///
    /// It is assumed a `Cargo.lock` is present.
    #[arg(long)]
    manifest_path: Option<PathBuf>,
    /// The platform tuples to do dependency resolution for
    ///
    /// Defaults to only the target tuple of the host if none are given.
    #[arg(short, long)]
    platform: Vec<String>,
    /// Only include resolutions for the platforms given with `--platform` for the main diff
    #[arg(short = 'P', long)]
    filter_to_platforms: bool,
    /// Run `cargo check` for updates
    ///
    /// This may potentially not be desirable since it will run build dependencies, though by
    /// default these are sandboxed (see the `--no-check-sandbox` option and all `--sandbox-...`
    /// options for configuring the sandbox and its details).
    ///
    /// Sandboxing requires the `--sandbox-rustwide-workspace` or `--sandbox-rustwide-workspace-tmp`
    /// flag to be set.
    #[arg(short = 'c', long)]
    check: bool,
    /// Require `check` to exit within this configured timeout (in seconds ('s') by default).
    ///
    /// This also supports the suffixes ms, min, h and d/day.
    #[arg(long, requires("check"))]
    check_timeout: Option<String>,
    /// Run `cargo check` without a sandbox.
    ///
    /// `cargo check` will run build dependencies, but rustwide (the sandboxing solution used by
    /// this program) requires Docker to work.
    ///
    /// You should probably not use this flag if you can't use sandboxing unless you're 100% sure
    /// you're fine with running untrusted code in your environment.
    #[arg(long, requires("check"))]
    check_no_sandbox: bool,
    /// The location of a permanent rustwide workspace.
    ///
    /// Notably this is NOT the source of the checked crate, and should not be contained therein
    /// either (the crate source will be copied there, among other things).
    #[arg(long, requires("check"))]
    sandbox_rustwide_workspace: Option<PathBuf>,
    /// Create a rustwide workspace in `/tmp`.
    ///
    /// Notably this will not work with `--sandbox-sibling-containers` unless `/tmp` was pointing to
    /// a host directory.
    #[arg(long, requires("check"), conflicts_with("sandbox_rustwide_workspace"))]
    sandbox_rustwide_workspace_tmp: bool,
    /// Use a local docker image for the rustwide sandbox
    #[arg(long, requires("check"), conflicts_with("check_no_sandbox"))]
    sandbox_image_local: Option<String>,
    /// Use a remote docker image from a registry for the rustwide sandbox
    #[arg(long, requires("check"), conflicts_with_all(["check_no_sandbox", "sandbox_image_local"]))]
    sandbox_image_remote: Option<String>,
    /// Prefer sandbox initialisation speed over runtime performance, by installing tools in the
    /// docker image in debug mode for example. This may be useful in CI environments.
    #[arg(long, requires("check"), conflicts_with("check_no_sandbox"))]
    sandbox_fast_init: bool,
    /// Use the hosts docker instance to create sibling containers for rustwide.
    ///
    /// This requires the docker socket (`/var/run/docker.sock`) to be mounted in the container this
    /// application runs in, and furthermore requires that the workspace directory is mounted
    /// somewhere in the host system, using workspaces created in a container is not supported.
    #[arg(long, requires("check"), conflicts_with("check_no_sandbox"))]
    sandbox_sibling_containers: bool,
    /// The rustup profile to use when installing toolchains in rustwide. The default is `minimal`.
    #[arg(long, requires("check"), conflicts_with("check_no_sandbox"))]
    sandbox_rustup_profile: Option<String>,
    /// Set a memory limit for the sandbox container.
    ///
    /// Set as a number with an optional suffix (default is in bytes (B), as well as K, M, G, T or
    /// KB/KiB etc)
    #[arg(long, requires("check"), conflicts_with("check_no_sandbox"))]
    sandbox_memory_limit: Option<String>,
    /// Set a CPU limit for the sandbox container as a (fractional) number of cores (ie 0.5 is half
    /// a core).
    #[arg(long, requires("check"), conflicts_with("check_no_sandbox"))]
    sandbox_cpu_limit: Option<f32>,
    /// Restrict the sandbox container to specific CPU IDs as a range split by '-' (translates to
    /// Dockers `--cpuset-cpus x-x`), ie `0-1` to select cores 0 & 1.
    #[arg(long, requires("check"), conflicts_with("check_no_sandbox"))]
    sandbox_cpuset_cpus: Option<String>,
    /// Enable network access in the sandbox container.
    #[arg(long, requires("check"), conflicts_with("check_no_sandbox"))]
    sandbox_enable_networking: bool,
    /// Do major updates (this edits `Cargo.toml` files)
    #[arg(short = 'm', long, requires("git"))]
    major: bool,
    /// Do major updates (this edits `Cargo.toml` files), but don't split minor and major updates
    /// into their own diffs
    #[arg(short = 'M', long, conflicts_with("major"))]
    squashed_major: bool,
    /// Create `git` commits or read a `git` repository
    #[arg(short, long)]
    git: bool,
    /// Don't do any updates, but compare from a specific git revision to the current one, or to
    /// `--to`
    #[arg(long, conflicts_with_all(["major", "squashed_major"]), requires("git"))]
    from: Option<String>,
    /// Don't do any updates, but compare until a specific git revision from the current one, or
    /// from `--from`
    #[arg(long, conflicts_with_all(["major", "squashed_major"]), requires("git"))]
    to: Option<String>,
    /// Produce templated output (or prettified JSON for missing templates)
    ///
    /// For `--major`, this concatenates the templates for the minor updates, and then the major
    /// update template per major update.
    #[arg(short, long)]
    templated: bool,
    /// Same as `--templated`, but use the squashed template format by taking a diff over all
    /// changes
    #[arg(short, long, requires("major"), conflicts_with("templated"))]
    templated_as_squashed: bool,
    /// Same as `--templated`, but render the templates into strings in a JSON object with more
    /// information
    ///
    /// This is also compatible with `--major`.
    #[arg(long, conflicts_with_all(["templated", "templated_as_squashed"]))]
    templated_in_json: bool,
    /// The path to a directory containing minijinja templates
    ///
    /// This option makes sense outside of `--templated`/`--templated-in-json`, because commits
    /// made using `--git` still use templating.
    ///
    /// The template names are:
    /// * `minor_commit.jinja`, `major_commit.jinja` and `squashed_commit.jinja` set the commit messages.
    /// * `minor_output.jinja`, `major_output.jinja`, `squashed_output.jinja` and `git_output.jinja` set the output data for the templated output with `--templated` or `--templated-in-json`.
    ///
    /// The JSON dump for outputs (without `--templated`) is always the same as the context the associated template gets.
    ///
    /// Extra context per template kind:
    /// * Output templates receive the commit hash if a new commit was made (via `--git`)
    /// * `major_commit.jinja` & `major_output.jinja`: `package` & `version` are both strings
    /// * `squashed_commit.jinja` & `squashed_output.jinja`: `major_updates` & `failed_major_updates` are both lists of objects with the keys `package` & `version`, pointing to strings each
    /// * `git_output.jinja`: `from` & `to` are both strings containing the commit hashes that were part of the comparison
    ///
    /// Extra functions implemented:
    /// * `short_platform` (filter): Removes the last segment if it remains unique, and all `unknown` segments from platform tuples
    ///
    /// Environment variables beginning with `RESOLVEDIFF_ENV_` (case insensitive) are also added as
    /// global variables with the `env_` prefix instead.
    /// The default template uses this to display the CI job that created a given update, using the
    /// `RESOLVEDIFF_ENV_CI_JOB_ID` and `RESOLVEDIFF_ENV_CI_JOB_URL` variables (both of which need
    /// to be present).
    #[arg(short = 'T', long, verbatim_doc_comment)]
    template_path: Option<PathBuf>,
}

#[derive(Clone)]
enum Task {
    Minor,
    Major,
    Squashed,
    Git {
        from: String,
        to: String,
        return_to: String,
    },
}

struct RunCheck<'a, 'b> {
    sandbox: Option<&'a rustwide::Build<'b>>,
    timeout: Option<Duration>,
}

struct AppContext<'a, 'b> {
    manifest_directory: PathBuf,
    lock_path: PathBuf,
    platforms: Vec<Platform>,
    include_all_platforms: bool,
    check: Option<RunCheck<'a, 'b>>,
    repository: Option<Repository>,
    output: OutputConfig,
    task: Task,
}

fn parse_suffixes(mut input: &str, suffixes: &[(&str, f64)]) -> Result<f64> {
    let mut out = 0.;
    while let Some(suffix_start) = input.find(|c| !matches!(c, '0'..='9' | '.')) {
        let (number, suffix_rest) = input.split_at(suffix_start);
        let (suffix, rest) = suffix_rest
            .find(|c| matches!(c, '0'..='9' | '.' | ' '))
            .map_or((suffix_rest, ""), |idx| suffix_rest.split_at(idx));
        input = rest.trim();

        let number = number.parse::<f64>()?;
        let (_, suffix_value) = suffixes
            .iter()
            .scan(1., |accumulated_value, (name, value)| {
                *accumulated_value *= value;
                Some((name, *accumulated_value))
            })
            .find(|(name, _)| suffix.eq_ignore_ascii_case(name))
            .ok_or_else(|| anyhow!("Unknown suffix {suffix:?}"))?;

        out += number * suffix_value;
    }

    if input.is_empty() {
        Ok(out)
    } else {
        Ok(out + input.parse::<f64>()?)
    }
}

/// Create a context
impl AppContext<'_, '_> {
    fn with_context_from<T>(
        args: Args,
        f: impl FnOnce(AppContext<'_, '_>) -> Result<T>,
    ) -> Result<T> {
        let manifest_path = args.manifest_path.map_or_else(locate_project, Ok)?;
        if manifest_path.extension() != Some("toml".as_ref()) {
            bail!("A manifest path should in \".toml\", found {manifest_path:?}");
        }

        let lock_path = manifest_path.with_extension("lock");

        let mut manifest_directory = manifest_path.canonicalize()?;
        assert!(manifest_directory.pop(), "there was a file name");

        let platforms = if args.platform.is_empty() {
            vec![host_platform()?]
        } else {
            args.platform.into_iter().map(Platform).collect::<Vec<_>>()
        };

        let check_timeout = args
            .check_timeout
            .map(|s| {
                parse_suffixes(
                    &s,
                    &[
                        ("ms", 0.001),
                        ("s", 1000.),
                        ("min", 60.),
                        ("h", 60.),
                        ("d", 24.),
                        ("day", 1.),
                    ],
                )
            })
            .transpose()?
            .map(Duration::from_secs_f64);

        let sandbox_memory_limit = args
            .sandbox_memory_limit
            .map(|s| {
                parse_suffixes(
                    &s,
                    &[
                        ("b", 1.),
                        ("k", 1024.),
                        ("kb", 1.),
                        ("kib", 1.),
                        ("m", 1024.),
                        ("mb", 1.),
                        ("mib", 1.),
                        ("g", 1024.),
                        ("gb", 1.),
                        ("gib", 1.),
                        ("t", 1024.),
                        ("tb", 1.),
                        ("tib", 1.),
                    ],
                )
            })
            .transpose()?
            .map(|float| float as usize);

        let sandbox_cpuset_cpus = args
            .sandbox_cpuset_cpus
            .map(|range| -> Result<_> {
                let (lower, upper) = range.split_once("-").unwrap_or((&range, &range));
                Ok(lower.parse::<usize>()?..=upper.parse()?)
            })
            .transpose()?;

        let mut repository = args
            .git
            .then(|| Repository::new(Some(manifest_directory.clone())));

        let output = OutputConfig {
            templated_output: args.templated || args.templated_as_squashed,
            templated_major_as_squashed: args.templated_as_squashed,
            templated_in_json: args.templated_in_json,
            jinja: OutputConfig::init_jinja(&platforms, args.template_path.as_deref())?,
        };

        let task = if args.major {
            Task::Major
        } else if args.squashed_major {
            Task::Squashed
        } else if args.from.is_some() || args.to.is_some() {
            let repository = repository.as_mut().expect("--from & --to require --git");

            let current = repository.current_branch_or_commit()?;
            let fix = |target: Option<_>| target.filter(|s| s != "HEAD").unwrap_or(current.clone());
            Task::Git {
                from: fix(args.from),
                to: fix(args.to),
                return_to: current,
            }
        } else {
            Task::Minor
        };

        let ctx = AppContext {
            manifest_directory,
            lock_path,
            platforms,
            include_all_platforms: !args.filter_to_platforms,
            check: None, // filled below
            repository,
            output,
            task,
        };

        if args.check {
            if !args.check_no_sandbox {
                eprintln!("Building workspace");

                let tmp_dir;
                let rustwide_dir = if let Some(ref dir) = args.sandbox_rustwide_workspace {
                    dir
                } else if args.sandbox_rustwide_workspace_tmp {
                    tmp_dir = tempfile::tempdir()?;
                    tmp_dir.path()
                } else {
                    bail!(
                        "Sandboxing requires --sandbox-rustwide-workspace \
                         or --sandbox-rustwide-workspace-tmp",
                    );
                };

                let mut builder =
                    rustwide::WorkspaceBuilder::new(rustwide_dir, "cargo-resolvediff")
                        .fast_init(args.sandbox_fast_init)
                        .running_inside_docker(args.sandbox_sibling_containers);

                if let Some(local) = args.sandbox_image_local {
                    builder = builder.sandbox_image(rustwide::cmd::SandboxImage::local(&local)?);
                } else if let Some(remote) = args.sandbox_image_remote {
                    builder = builder.sandbox_image(rustwide::cmd::SandboxImage::remote(&remote)?);
                }

                if let Some(profile) = args.sandbox_rustup_profile {
                    builder = builder.rustup_profile(&profile);
                }

                let workspace = builder.init()?;

                let name = ctx
                    .manifest_directory
                    .file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .unwrap_or("project");
                let random = uuid::Uuid::new_v4();
                let mut build_dir = workspace.build_dir(&format!("{name}-{random}"));

                let sandbox = rustwide::cmd::SandboxBuilder::new()
                    .memory_limit(sandbox_memory_limit)
                    .cpu_limit(args.sandbox_cpu_limit)
                    .cpuset_cpus(sandbox_cpuset_cpus)
                    .enable_networking(args.sandbox_enable_networking);

                let toolchain =
                    rustwide::Toolchain::dist(&detect_toolchain(&ctx.manifest_directory)?);
                let krate = rustwide::Crate::local(&ctx.manifest_directory);
                krate.fetch(&workspace)?;

                let out = build_dir
                    .build(&toolchain, &krate, sandbox)
                    .run(|sandbox| {
                        Ok(f(AppContext {
                            check: Some(RunCheck {
                                sandbox: Some(sandbox),
                                timeout: check_timeout,
                            }),
                            ..ctx
                        }))
                    })
                    .and_then(|result| result.into_inner());

                build_dir.purge().and(out)
            } else {
                f(AppContext {
                    check: Some(RunCheck {
                        sandbox: None,
                        timeout: check_timeout,
                    }),
                    ..ctx
                })
            }
        } else {
            f(ctx)
        }
    }
}

struct MajorUpdateContext {
    manifest_deps: ManifestDependencySet,
    client: SyncClient,
}

impl MajorUpdateContext {
    fn new(resolved: &Resolved) -> Result<(Self, Vec<String>)> {
        let manifest_deps = ManifestDependencySet::collect(&resolved.full_metadata)?;
        let direct_dependencies = manifest_deps.dependencies.keys().cloned().collect();

        let client = SyncClient::new(
            "cargo-resolvediff (42triangles@tutanota.com)",
            std::time::Duration::from_millis(1000),
        )?;

        let ctx = MajorUpdateContext {
            manifest_deps,
            client,
        };
        Ok((ctx, direct_dependencies))
    }

    fn update_for(&mut self, name: String) -> Result<Option<SpecificCrateIdent>> {
        let mentions = self
            .manifest_deps
            .dependencies
            .get_mut(&name)
            .expect("Key should have been collected from that map");

        let version = match fetch_latest_major_update_for(
            &self.client,
            &name,
            mentions.iter().map(|mention| mention.version()),
        )? {
            LatestVersion::CrateNotFound | LatestVersion::NoMajorUpdates => return Ok(None),
            LatestVersion::NewestUpdate(version) => version,
        };

        let crate_version = SpecificCrateIdent { name, version };

        self.manifest_deps
            .manifests
            .update_versions_in_file(mentions, &crate_version.version)?;

        Ok(Some(crate_version))
    }

    fn git_commit_after_update(
        &self,
        lock: &Path,
        repository: &mut Repository,
        message: &str,
    ) -> Result<String> {
        repository.add(lock)?;
        for manifest in self.manifest_deps.manifests.as_slice() {
            repository.add(manifest.path())?;
        }

        let commit = repository
            .commit(message)?
            .expect("There should have been changes after a major update");
        Ok(commit)
    }
}

#[derive(Serialize)]
struct MajorUpdates {
    minor: serde_json::Value,
    major_order: Vec<SpecificCrateIdent>,
    major_updates: BTreeMap<String, serde_json::Value>,
    failed_major_updates: Vec<SpecificCrateIdent>,
}

/// Implementation of the actual program
impl AppContext<'_, '_> {
    fn try_update_lockfile_and_check(&self) -> Result<bool> {
        if !fetch(&self.manifest_directory)? {
            return Ok(false);
        }

        if let Some(ref run_check) = self.check {
            check(
                &self.manifest_directory,
                run_check.sandbox,
                run_check.timeout,
            )
        } else {
            Ok(true)
        }
    }

    fn minor_update(&self) -> Result<()> {
        eprintln!("Doing minor updates");

        if !update(&self.manifest_directory)? || !self.try_update_lockfile_and_check()? {
            bail!("Minor updates failed");
        }

        Ok(())
    }

    fn resolve(&self) -> Result<Resolved> {
        eprintln!("Collecting cargo resolution metadata");
        Resolved::resolve_from_path(
            &self.manifest_directory,
            self.platforms.iter().cloned(),
            self.include_all_platforms,
        )
    }

    fn minor_update_task(&mut self) -> Result<(Resolved, serde_json::Value)> {
        let before = self.resolve()?;
        self.minor_update()?;
        let after = self.resolve()?;

        let diff = Diff::between(&before, &after);

        let commit = if let Some(ref mut repo) = self.repository {
            repo.add(&self.lock_path)?;
            repo.commit(&self.output.minor_commit(&diff)?)?
        } else {
            None
        };

        let output = self.output.minor_output(&diff, commit.as_deref())?;
        Ok((after, output))
    }

    fn major_update_task(&mut self) -> Result<serde_json::Value> {
        let first = self.resolve()?;
        let mut last_owned;
        let mut last = &first;

        let (mut major_ctx, direct_dependencies) = MajorUpdateContext::new(&first)?;

        let mut major_order = Vec::new();
        let mut major_updates = BTreeMap::new();
        let mut failed_major_updates = Vec::new();

        major_ctx.manifest_deps.commit()?;

        for package in direct_dependencies {
            eprintln!("Updating {package}");

            major_ctx.manifest_deps.roll_back()?;

            let Some(package) = major_ctx.update_for(package)? else {
                continue;
            };

            if !self.try_update_lockfile_and_check()? {
                failed_major_updates.push(package);
                continue;
            };

            let resolve = self.resolve()?;
            let diff = Diff::between(last, &resolve);

            let message = self
                .output
                .major_commit(&diff, &package.name, &package.version)?;

            let repository = self
                .repository
                .as_mut()
                .expect("Split major updates require a git repository");
            let commit =
                major_ctx.git_commit_after_update(&self.lock_path, repository, &message)?;

            let output =
                self.output
                    .major_output(&diff, &package.name, &package.version, Some(&commit))?;

            major_ctx.manifest_deps.commit()?;

            major_order.push(package.clone());
            major_updates.insert(package.name, output);

            last_owned = resolve;
            last = &last_owned;
        }

        let (last, minor) = self.minor_update_task()?;

        let squashed_diff = Diff::between(&first, &last);

        self.output.merged_major_output(
            &squashed_diff,
            &MajorUpdates {
                minor,
                major_order,
                major_updates,
                failed_major_updates,
            },
        )
    }

    fn squashed_update_task(&mut self) -> Result<serde_json::Value> {
        let before = self.resolve()?;

        let (mut major_ctx, direct_dependencies) = MajorUpdateContext::new(&before)?;

        let mut major_updates = Vec::new();
        let mut failed_major_updates = Vec::new();

        major_ctx.manifest_deps.commit()?;
        for package in direct_dependencies {
            eprintln!("Checking {package}");

            major_ctx.manifest_deps.roll_back()?;

            let Some(package) = major_ctx.update_for(package)? else {
                eprintln!("Skipping, since there are no relevant updates");
                continue;
            };

            if !self.try_update_lockfile_and_check()? {
                failed_major_updates.push(package);
                eprintln!("Failed");
                continue;
            };

            major_ctx.manifest_deps.commit()?;
            major_updates.push(package);
            eprintln!("Succeeded");
        }

        self.minor_update()?;

        let after = self.resolve()?;
        let diff = Diff::between(&before, &after);

        let message = self
            .output
            .squashed_commit(&diff, &major_updates, &failed_major_updates)?;

        let commit = self
            .repository
            .as_mut()
            .map(|repository| {
                major_ctx.git_commit_after_update(&self.lock_path, repository, &message)
            })
            .transpose()?;

        let output = self.output.squashed_output(
            &diff,
            &major_updates,
            &failed_major_updates,
            commit.as_deref(),
        )?;
        Ok(output)
    }

    fn git_task(&mut self, from: &str, to: &str, return_to: &str) -> Result<serde_json::Value> {
        let mut repository = self
            .repository
            .take()
            .expect("git comparisons require a repository");

        repository.checkout(from)?;
        let from_commit = repository.current_commit()?;
        let from = self.resolve()?;

        repository.checkout(return_to)?;
        repository.checkout(to)?;
        let to_commit = repository.current_commit()?;
        let to = self.resolve()?;

        repository.checkout(return_to)?;

        self.repository = Some(repository);
        let output =
            self.output
                .git_output(&Diff::between(&from, &to), &from_commit, &to_commit)?;
        Ok(output)
    }
}

fn main() -> Result<()> {
    AppContext::with_context_from(Args::parse(), |mut ctx| {
        let out = match ctx.task.clone() {
            Task::Minor => ctx.minor_update_task()?.1,
            Task::Major => ctx.major_update_task()?,
            Task::Squashed => ctx.squashed_update_task()?,
            Task::Git {
                from,
                to,
                return_to,
            } => ctx.git_task(&from, &to, &return_to)?,
        };

        ctx.output.final_output(&out)?;

        Ok(())
    })
}
