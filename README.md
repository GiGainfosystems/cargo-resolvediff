# `cargo-resolvediff`
This program does both minor updates (using `cargo update`) and major updates (by editing the `Cargo.toml`s in the workspace), and produces review diffs between each step for the dependency resolution for the given platforms.

This allows for reviewing all changes in your dependences (minus `git` dependencies, see the next section), without reviewing changes for crates that you don't ever build when `--filter-to-plaforms` is enabled.

See [`example-output-squashed.md`](example-output-squashed.md) as an example.

This crate is also published on [crates.io](https://crates.io/crates/cargo-resolvediff) and can be installed using `cargo install cargo-resolvediff`.

## Warning about `git` dependencies
`git` dependencies that don't pin a specific commit & aren't under the users control will not show up in the diff currently, which means that you'd have to check them manually.

## Pitfalls when filtering to platforms
As is, this crate does not have the capability to diff (well) between a version without a platform added and one with that platform added or removed, if `--filter-to-platforms` is enabled.

It also currently does not run `cargo check` for any platform except the one the target crate/workspace defaults to.

## Pitfalls for dependencies that ought to be kept in sync
As is, dependencies for which versions must be kept in sync are not supported, since the automatic major update mechanism always only handles one crate at a time. A manual update and then comparing using `--git --from` is, however, possible.

## Pinning dependencies
Dependencies are considered "pinned" & not updated as major updates automatically if they're specified using `<`, `<=` or `=` semver operators. Pinning versions in the Cargo.lock is currently not possible.

## Usage
```
A tool for diffing `cargo` dependency resolutions between updates

Usage: cargo-resolvediff [OPTIONS]

Options:
      --manifest-path <MANIFEST_PATH>
          The path to the manifest of the workspace to update

          It is assumed a `Cargo.lock` is present.

  -p, --platform <PLATFORM>
          The platform tuples to do dependency resolution for

          Defaults to only the target tuple of the host if none are given.

  -P, --filter-to-platforms
          Only include resolutions for the platforms given with `--platform`
          for the main diff

  -c, --check
          Run `cargo check` for updates

          This may potentially not be desirable since it will run build dependencies,
          though by default these are sandboxed
          (see the `--no-check-sandbox` option and all `--sandbox-...` options
          for configuring the sandbox and its details).

          Sandboxing requires the `--sandbox-rustwide-workspace`
          or `--sandbox-rustwide-workspace-tmp` flag to be set.

      --check-timeout <CHECK_TIMEOUT>
          Require `check` to exit within this configured timeout
          (in seconds ('s') by default).

          This also supports the suffixes ms, min, h and d/day.

      --check-no-sandbox
          Run `cargo check` without a sandbox.

          `cargo check` will run build dependencies,
          but rustwide (the sandboxing solution used by this program) requires Docker
          to work.

          You should probably not use this flag if you can't use sandboxing
          unless you're 100% sure you're fine with running untrusted code in your
          environment.

      --sandbox-rustwide-workspace <SANDBOX_RUSTWIDE_WORKSPACE>
          The location of a permanent rustwide workspace.

          Notably this is NOT the source of the checked crate,
          and should not be contained therein either
          (the crate source will be copied there, among other things)

      --sandbox-rustwide-workspace-tmp
          Create a rustwide workspace in `/tmp`.

          Notably this will not work with `--sandbox-sibling-containers`
          unless `/tmp` was pointing to a host directory.

      --sandbox-image-local <SANDBOX_IMAGE_LOCAL>
          Use a local docker image for the rustwide sandbox

      --sandbox-image-remote <SANDBOX_IMAGE_REMOTE>
          Use a remote docker image from a registry for the rustwide sandbox

      --sandbox-fast-init
          Prefer sandbox initialisation speed over runtime performance,
          by installing tools in the docker image in debug mode for example.
          This may be useful in CI environments

      --sandbox-sibling-containers
          Use the hosts docker instance to create sibling containers for rustwide.

          This requires the docker socket (`/var/run/docker.sock`) to be mounted
          in the container this application runs in,
          and furthermore requires that the workspace directory is mounted somewhere
          in the host system, using workspaces created in a container is not supported

      --sandbox-rustup-profile <SANDBOX_RUSTUP_PROFILE>
          The rustup profile to use when installing toolchains in rustwide.
          The default is `minimal`

      --sandbox-memory-limit <SANDBOX_MEMORY_LIMIT>
          Set a memory limit for the sandbox container.

          Set as a number with an optional suffix
          (default is in bytes (B), as well as K, M, G, T or KB/KiB etc)

      --sandbox-cpu-limit <SANDBOX_CPU_LIMIT>
          Set a CPU limit for the sandbox container as a (fractional) number of cores
          (ie 0.5 is half a core)

      --sandbox-cpuset-cpus <SANDBOX_CPUSET_CPUS>
          Restrict the sandbox container to specific CPU IDs as a range split by '-'
          (translates to Dockers `--cpuset-cpus x-x`), ie `0-1` to select cores 0 & 1

      --sandbox-enable-networking
          Enable network access in the sandbox container

  -m, --major
          Do major updates (this edits `Cargo.toml` files)

  -M, --squashed-major
          Do major updates (this edits `Cargo.toml` files),
          but don't split minor and major updates into their own diffs

  -g, --git
          Create `git` commits or read a `git` repository

      --from <FROM>
          Don't do any updates,
          but compare from a specific git revision to the current one, or to `--to`

      --to <TO>
          Don't do any updates,
          but compare until a specific git revision from the current one, or from `--from`

  -t, --templated
          Produce templated output (or prettified JSON for missing templates)

          For `--major`, this concatenates the templates for the minor updates,
          and then the major update template per major update.

  -t, --templated-as-squashed
          Same as `--templated`,
          but use the squashed template format by taking a diff over all changes

      --templated-in-json
          Same as `--templated`,
          but render the templates into strings in a JSON object with more information

          This is also compatible with `--major`.

  -T, --template-path <TEMPLATE_PATH>
          The path to a directory containing minijinja templates

          This option makes sense outside of `--templated`/`--templated-in-json`,
          because commits made using `--git` still use templating.

          The template names are:
          * `minor_commit.jinja`, `major_commit.jinja` and `squashed_commit.jinja`
            set the commit messages.
          * `minor_output.jinja`, `major_output.jinja`, `squashed_output.jinja` and
            `git_output.jinja` set the output data for the templated output
            with `--templated` or `--templated-in-json`.

          The JSON dump for outputs (without `--templated`) is always the same
          as the context the associated template gets.

          Extra context per template kind:
          * Output templates receive the commit hash if a new commit was made
            (via `--git`)
          * `major_commit.jinja` & `major_output.jinja`:
            `package` & `version` are both strings
          * `squashed_commit.jinja` & `squashed_output.jinja`:
            `major_updates` & `failed_major_updates` are both lists of objects
            with the keys `package` & `version`, pointing to strings each
          * `git_output.jinja`: `from` & `to` are both strings containing
            the commit hashes that were part of the comparison

          Extra functions implemented:
          * `short_platform` (filter): Removes the last segment if it remains unique,
            and all `unknown` segments from platform tuples

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

The default templates can be found at [`src/default_templates/`](src/default_templates).

## Notes about the implementation
Most places use `BTreeMap`s & `BTreeSet`s for their deterministic iteration order (& corresponding sorted JSON output).

## License
* Apache License, Version 2.0 (<https://www.apache.org/licenses/LICENSE-2.0>).
* MIT License (<https://opensource.org/licenses/MIT>)
