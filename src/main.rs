// Copyright (C) 2026 by GiGa infosystems

// NOTE: This doesn't handle `git` dependencies currently, as they cannot really be detected in
// `cargo metadata` outside of parsing the source.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use clap::Parser;
use color_eyre::{
    Result,
    eyre::{Report, bail},
};
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
use cargo_resolvediff::util::{host_platform, locate_project, update};

enum TemplateContext {
    Minijinja {
        path: PathBuf,
        jinja: Box<minijinja::Environment<'static>>,
    },
    OnlyDefaults,
}

impl TemplateContext {
    fn init(platforms: &[Platform], path: Option<PathBuf>) -> Result<Self> {
        match path {
            None => Ok(TemplateContext::OnlyDefaults),
            Some(path) => {
                if !path.is_dir() {
                    bail!("Template directory doesn't exist");
                }

                let mut jinja = minijinja::Environment::new();
                jinja.set_loader(minijinja::path_loader(&path));

                let short_platform = {
                    let mapping = platforms
                        .iter()
                        .map(|platform| {
                            let short = if let Some((short, _)) = platform.0.rsplit_once("-")
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

                Ok(TemplateContext::Minijinja {
                    path,
                    jinja: Box::new(jinja),
                })
            }
        }
    }

    fn render(&self, name: &str, ctx: &impl Serialize) -> Result<Option<String>> {
        match self {
            TemplateContext::Minijinja { path, jinja } if path.join(name).is_file() => {
                Ok(Some(jinja.get_template(name)?.render(ctx)?))
            }
            TemplateContext::Minijinja { .. } | TemplateContext::OnlyDefaults => Ok(None),
        }
    }

    fn render_output(&self, name: &str, ctx: &impl Serialize) -> Result<String> {
        match self.render(name, ctx)? {
            Some(out) => Ok(out),
            None => Ok(serde_json::to_string_pretty(ctx)?),
        }
    }
}

struct OutputConfig {
    templated_output: bool,
    templated_in_json: bool,
    template_ctx: TemplateContext,
}

impl OutputConfig {
    const MINOR_COMMIT: &str = "minor_commit.jinja";
    const MINOR_OUTPUT: &str = "minor_output.jinja";
    const MAJOR_COMMIT: &str = "major_commit.jinja";
    const MAJOR_OUTPUT: &str = "major_output.jinja";
    const SQUASHED_COMMIT: &str = "squashed_commit.jinja";
    const SQUASHED_OUTPUT: &str = "squashed_output.jinja";

    fn output(
        &self,
        name: &str,
        ctx: minijinja::Value,
        commit: Option<&str>,
    ) -> Result<serde_json::Value> {
        let ctx = minijinja::context! {
            commit => commit,
            ..ctx
        };

        if self.templated_output {
            Ok(self.template_ctx.render_output(name, &ctx)?.into())
        } else {
            Ok(serde_json::to_value(&ctx)?)
        }
    }

    fn minor_commit(&self, diff: &Diff<'_>) -> Result<String> {
        let out = self
            .template_ctx
            .render(Self::MINOR_COMMIT, diff)?
            .unwrap_or_else(|| {
                "Automatic minor dependency updates using `cargo update`".to_owned()
            });
        Ok(out)
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
            .template_ctx
            .render(
                Self::MAJOR_COMMIT,
                &Self::major_context(diff, package, version),
            )?
            .unwrap_or_else(|| {
                format!("Automatic major dependency update of `{package}` to `{version}`")
            });
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
        let out = self
            .template_ctx
            .render(
                Self::SQUASHED_COMMIT,
                &Self::squashed_context(diff, major_updates, failed_major_updates),
            )?
            .unwrap_or_else(|| "Automatic dependency updates".to_owned());
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

    fn final_output(&self, value: &serde_json::Value) -> Result<()> {
        if !self.templated_in_json {
            println!(
                "{}",
                value
                    .as_str()
                    .expect("Was templated, and as such is always a string")
            );
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
    /// This may potentially not be desirable since it will run build dependencies.
    #[arg(short = 'c', long)]
    check: bool,
    /// Do major updates (this edits `Cargo.toml` files)
    #[arg(short = 'm', long, requires("git"))]
    major: bool,
    /// Do major updates (this edits `Cargo.toml` files), but don't split minor and major updates
    /// into their own diffs
    #[arg(short = 'M', long, conflicts_with("major"))]
    squashed_major: bool,
    /// Create `git` commits
    #[arg(short, long)]
    git: bool,
    /// Produce templated output (or prettified JSON for missing templates)
    #[arg(short, long, conflicts_with("major"))]
    templated: bool,
    /// Same as `--templated`, but render the templates into strings in a JSON object with more
    /// information
    ///
    /// This is also compatible with `--major`.
    #[arg(long, conflicts_with("templated"))]
    templated_in_json: bool,
    /// The path to a directory containing minijinja templates
    ///
    /// This option makes sense outside of `--templated`/`--templated-in-json`, because commits
    /// made using `--git` still use templating.
    ///
    /// The template names are:
    /// * `minor_commit.jinja`, `major_commit.jinja` and `squashed_commit.jinja` set the commit messages.
    ///   The defaults are "Automatic minor dependency updates using `cargo update`", "Automatic major dependency update of `{package}` to `{version}`" and "Automatic dependency updates" respectively.
    /// * `minor_output.jinja`, `major_output.jinja` and `squashed_output.jinja` set the output data for the templated output with `--templated` or `--templated-in-json`.
    ///   The default is just a prettified JSON dump.
    ///
    /// The prettified JSON dump is always the same as the context the associated template gets.
    ///
    /// Extra context per template kind:
    /// * Output templates receive the commit hash if a new commit was made (via `--git`)
    /// * `major_commit.jinja` & `major_output.jinja`: `package` & `version` are both strings
    /// * `squashed_commit.jinja` & `squashed_output.jinja`: `major_updates` & `failed_major_updates` are both lists of objects with the keys `package` & `version`, pointing to strings each
    ///
    /// Extra functions implemented:
    /// * `short_platform` (filter): Removes the last segment if it remains unique, and all `unknown` segments from platform tuples
    #[arg(short = 'T', long, verbatim_doc_comment)]
    template_path: Option<PathBuf>,
}

enum Task {
    Minor,
    Major,
    Squashed,
}

struct AppContext {
    manifest_path: PathBuf,
    lock_path: PathBuf,
    platforms: Vec<Platform>,
    include_all_platforms: bool,
    check: bool,
    repository: Option<Repository>,
    output: OutputConfig,
    task: Task,
}

impl TryFrom<Args> for AppContext {
    type Error = Report;

    fn try_from(args: Args) -> Result<Self> {
        let manifest_path = args.manifest_path.map_or_else(locate_project, Ok)?;
        if manifest_path.extension() != Some("toml".as_ref()) {
            bail!("A manifest path should in \".toml\", found {manifest_path:?}");
        }

        let lock_path = manifest_path.with_extension("lock");

        let platforms = if args.platform.is_empty() {
            vec![host_platform()?]
        } else {
            args.platform.into_iter().map(Platform).collect::<Vec<_>>()
        };

        let repository = args.git.then(|| {
            let repository_path = manifest_path.parent().expect("there was a file name");
            // We might already be in the directory with the `Cargo.toml`, in which case `git`
            // commands can run here:
            let repository_path = (repository_path != "").then(|| repository_path.to_owned());
            Repository::new(repository_path)
        });

        let output = OutputConfig {
            templated_output: args.templated || args.templated_in_json,
            templated_in_json: args.templated_in_json,
            template_ctx: TemplateContext::init(&platforms, args.template_path)?,
        };

        let task = if args.major {
            Task::Major
        } else if args.squashed_major {
            Task::Squashed
        } else {
            Task::Minor
        };

        Ok(AppContext {
            manifest_path,
            lock_path,
            platforms,
            include_all_platforms: !args.filter_to_platforms,
            check: args.check,
            repository,
            output,
            task,
        })
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
    major_order: Vec<String>,
    major_updates: BTreeMap<String, serde_json::Value>,
    failed_major_updates: Vec<SpecificCrateIdent>,
}

impl AppContext {
    fn try_update(&self) -> Result<bool> {
        update(&self.manifest_path, self.check)
    }

    fn minor_update(&self) -> Result<()> {
        if !self.try_update()? {
            bail!("Minor updates failed");
        }

        Ok(())
    }

    fn resolve(&self) -> Result<Resolved> {
        Resolved::resolve_from_path(
            &self.manifest_path,
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

    fn major_update_task(&mut self) -> Result<MajorUpdates> {
        let (mut last, minor) = self.minor_update_task()?;

        let (mut major_ctx, direct_dependencies) = MajorUpdateContext::new(&last)?;

        let mut major_order = Vec::new();
        let mut major_updates = BTreeMap::new();
        let mut failed_major_updates = Vec::new();

        major_ctx.manifest_deps.commit()?;

        for package in direct_dependencies {
            major_ctx.manifest_deps.roll_back()?;

            let Some(package) = major_ctx.update_for(package)? else {
                continue;
            };

            if !self.try_update()? {
                failed_major_updates.push(package);
                continue;
            };

            let resolve = self.resolve()?;
            let diff = Diff::between(&last, &resolve);

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

            major_order.push(package.name.clone());
            major_updates.insert(package.name, output);

            last = resolve;
        }

        Ok(MajorUpdates {
            minor,
            major_order,
            major_updates,
            failed_major_updates,
        })
    }

    fn squashed_update_task(&mut self) -> Result<serde_json::Value> {
        let before = self.resolve()?;

        self.minor_update()?;

        let (mut major_ctx, direct_dependencies) = MajorUpdateContext::new(&before)?;

        let mut major_updates = Vec::new();
        let mut failed_major_updates = Vec::new();

        major_ctx.manifest_deps.commit()?;
        for package in direct_dependencies {
            major_ctx.manifest_deps.roll_back()?;

            let Some(package) = major_ctx.update_for(package)? else {
                continue;
            };

            if !self.try_update()? {
                failed_major_updates.push(package);
                continue;
            };

            major_ctx.manifest_deps.commit()?;
            major_updates.push(package);
        }

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
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let mut ctx = AppContext::try_from(Args::parse())?;

    let out = match ctx.task {
        Task::Minor => ctx.minor_update_task()?.1,
        Task::Major => {
            let out = ctx.major_update_task()?;
            output_json(&out)?;
            return Ok(());
        }
        Task::Squashed => ctx.squashed_update_task()?,
    };

    ctx.output.final_output(&out)?;

    Ok(())
}
