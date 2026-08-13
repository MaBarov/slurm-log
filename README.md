# slurm-log

Fast, owner-filtered SLURM log browser written in Rust. It discovers local and
remote jobs, presents a searchable terminal picker, and follows selected logs
in tiled tmux panes. It supports arrays, pending jobs, archives, warning
filtering, pane management, automatic job addition, mouse copying, lifecycle
alerts, a live CPU/memory/GPU details dashboard, failure/resource summaries,
an MCP server for AI clients, and a private per-user acceleration daemon.

Each installation is isolated by Unix user. Configuration, job history,
daemon sockets, and caches are never shared between users.

## Quick setup

Requirements:

- Linux with `tmux`
- `squeue` and `scontrol` for local clusters; `ssh` for remote clusters
- Rust/Cargo only when rebuilding instead of using the bundled Linux binary
- `sacct` only on clusters where accounting is enabled

Install a signed x86-64 Linux release only after obtaining its Ed25519 public
key PEM from an independent, trusted channel (for example, a reviewed source
commit or an organization-controlled deployment record):

```bash
sh install.sh --release-public-key /secure/path/slurm-log-release-public.pem
```

The installer requires that PEM for every prebuilt download. It verifies the
detached manifest signature before downloading the archive, then checks the
signed size and digest before extraction or starting the candidate binary. Do
not obtain the PEM from the same release URL, archive, or mutable mirror being
verified. The script does not require Rust. To pin a release, use
`sh install.sh --version v0.2.4 --release-public-key /secure/path/key.pem`.

Alternatively, extract a shared release archive and run:

```bash
tar -xzf slurm-log-linux-x86_64.tar.gz
cd slurm-log
./install.sh
```

The installer does not assume a particular site or cluster. Its setup wizard
lets each user add only the local and SSH clusters they actually use;
an SSH host can be a hostname or an alias from `~/.ssh/config`. The installer:

1. checks runtime dependencies;
2. uses a bundled binary, downloads a verified release, or builds with
   `--build` when requested;
3. installs it to `~/.local/bin/slurm-log`;
4. creates a private configuration and state directory;
5. starts the interactive cluster and sbatch-bank setup wizard;
6. explains how to add `~/.local/bin` to `PATH` when necessary.

Run `./install.sh --help` for pinned versions, source builds, custom prefixes,
state paths, prebuilt binaries, configuration replacement, and the
noninteractive `--no-setup` option.

## First run

```bash
slurm-log
```

Press `?` inside the picker for the complete command reference. Common keys:

- arrows or `j`/`k`: navigate;
- Space: select;
- Enter: open selected logs;
- `o`: cycle jobs from the last 2h, 12h, 1 day, 1 week, all history, or live;
- `a`: toggle directly between all accounting history and live jobs;
- `/`: search;
- `i`: inspect the focused job's live resource details;
- `q`: quit.

Inside the tmux workspace, `Ctrl-b j` manages open panes, `Ctrl-b A` toggles
automatic additions, `Ctrl-b i` toggles a small details pane attached to the
focused log, and
`Ctrl-b q` closes a one-log-panel workspace immediately and asks
for confirmation only when multiple log panels are open.

## Job details

```bash
slurm-log details JOB_ID --cluster YOUR_CLUSTER_NAME
```

The interactive dashboard samples active-job metrics at most once every 30
seconds and shows scheduling, requested and allocated CPUs, memory, nodes and
GPUs, CPU/memory efficiency,
available GPU accounting, placement, exit information, and short in-session
trends. Press Space to pause, `r` to refresh, or `q`, Esc, or Enter to close.
Finished jobs freeze on their final accounting snapshot. When output is
redirected, the command prints one stable plain-text snapshot.

GPU allocation is always reported when present in Slurm TRES. Actual GPU
utilization is shown only when the cluster records `gpuutil`/`gpumem` counters.

## MCP server

Any local MCP client that supports stdio can launch the standards-compatible
server directly:

```bash
slurm-log mcp
```

`slurm-log mcp serve` is an explicit alias. The process writes only MCP
JSON-RPC to stdout and exits cleanly at EOF; it never opens an HTTP port.
Cluster configuration and tool schemas are validated and snapshotted when the
process starts, so reconnect the client after changing clusters. Bank tools
reload the validated bank-root list and automatically detect added, removed, or
changed `.sbatch` files without reconnecting.

For Codex and Claude Code, guided user-scoped setup shows the exact registration
command before asking whether to run it, preserves existing registrations unless
replacement is explicitly accepted, and verifies the result:

```bash
slurm-log mcp setup
slurm-log mcp doctor
slurm-log mcp unregister
```

Setup also prints portable JSON for Cursor, VS Code, Windsurf, Cline, and other
stdio clients. It never edits an editor configuration or enables automatic tool
approval. The generic shape is:

```json
{
  "mcpServers": {
    "slurm-log": {
      "type": "stdio",
      "command": "/absolute/path/slurm-log",
      "args": ["mcp"]
    }
  }
}
```

The server exposes owner-scoped tools for clusters, jobs, exact job inspection,
tmux workspace context, bounded log reads/search/diagnosis, configured script
banks, submission, and cancellation. Job inspection and logs always require an
exact `{cluster, job_id}` pair. Submission never guesses a cluster and uses a
five-minute, one-use preview token bound to the selected bank script, target,
directives, working directory, job name, and SHA-256 digest. Cancellation
performs fresh queue and controller owner/name/state checks immediately before
`scancel`: it accepts an ordinary job or one controller-proven `MASTER_TASK`
array task, and rejects array masters or ranges rather than risking array-wide
scope.
Mutation tools carry mutation/destructive MCP annotations and depend on the
client to request approval.

Resources are available at `slurm-log://clusters`, per-cluster job lists, and
cluster-qualified job, details, and log URIs. Concrete resources support MCP
subscribe/unsubscribe notifications that fire only when sampled data changes;
clients without subscriptions can use cursor-based log reads. Returned log text
is explicitly marked as untrusted data, stripped of terminal control sequences,
and bounded to recent Slurm `StdOut` data. `StdOut` is resolved only beneath the
configured cluster working directory; a path outside that root is treated as
unavailable. No tool accepts a filesystem path, shell command, script body,
sbatch option, SSH credential, or another user's job.
Mutation attempts are recorded without contents or credentials in a private,
rotating `mcp-audit.jsonl` beside the state ledger.

## Configuration

Configuration is stored at `~/.config/slurm-log/config.json` by default:

```json
{
  "clusters": [
    {"name":"local-lab","transport":"local","user":"local-user","workingDirectory":"/home/user/project","accounting":false},
    {"name":"remote-lab","controller":"production-a","transport":"ssh","user":"remote-user","sshHost":"cluster-alias","workingDirectory":"/home/user/project","accounting":true}
  ],
  "sbatchBanks": [
    {"path":"/home/user/project/cluster"},
    {"path":"/home/user/shared-jobs","name":"Shared Jobs"}
  ],
  "statePath": "/home/user/.local/state/slurm-log/state.json"
}
```

Run `slurm-log setup` for the per-user wizard. On a fresh configuration it
makes no cluster assumptions. Existing explicit cluster settings are offered
as editable defaults. For an SSH cluster, setup presents literal aliases from
`~/.ssh/config` (including common `Include` files) in an arrow-key picker. Once
selected, one bounded noninteractive SSH probe detects the remote SLURM cluster
name, user, home directory, and `sacct` availability; these are shown and used
as editable defaults. If connection or detection fails, setup continues with
manual fields.

`name` is the local display label. `controller` is the Slurm controller
identity used to bind remote scheduler queries, submissions, and cancellations;
it may differ from the label. Existing configurations without `controller`
remain valid and use `name` as their controller identity. Remote submission
requires `sbatch --parsable` to return that same controller identity, and a
script whose `#SBATCH --clusters`/`-M` directive names another controller is
rejected before preview or submission.

Bank discovery proposes local per-user roots that actually exist, including
the home directory, common storage/scratch/work mounts, and `SCRATCH`, `WORK`,
`PROJECT_DIR`, or `PROJECTS` environment paths. The list remains editable
before scanning, so installations with different filesystem layouts do not
inherit cluster-specific assumptions.

Setup defaults the small state ledger and daemon socket to responsive storage
under `~/.local/state/slurm-log`; avoid placing them on a cluster/network mount.

Set `"accounting": false` when a cluster does not provide Slurm accounting.
slurm-log then avoids `sacct` entirely and uses `squeue`, `scontrol`, and
`sstat` for active jobs. Completed-job history is not available from Slurm on
such clusters.

Press `s` in the job picker (or run `slurm-log bank`) to browse `.sbatch`
files recursively. During setup, enter one or more broad workspace roots and
slurm-log discovers Git repositories containing `.sbatch` files; loose scripts
are grouped under the first containing directory below a broad search root, so
`$HOME` itself does not become a bank. You can select all or individual results
and add manual/custom-named banks when needed. Setup marks Git repositories
with a cyan `GIT` label and loose directories with a yellow `FOLDER` label.
Discovery and bank loading inspect at most three directory levels. Missing
banks can be selected with the mouse/keyboard folder browser. Bank scans use
isolated helpers with a three-second total budget, so a stalled mount cannot
freeze the picker. Successful catalogs are cached privately for 30 seconds;
press `r` for a guaranteed fresh scan. Discovery does not follow symlinks and skips build/cache trees. Banks and folders are
collapsible. A bank's explicit `name`
wins; otherwise slurm-log uses the enclosing Git repository name, or finally
the bank directory name. Duplicate inferred names receive a stable ` (2)`
suffix. Enter chooses a script, then a cluster, shows its `#SBATCH` arguments
and working directory, and requires `y` before submission. After
`sbatch --parsable` succeeds, choose whether to open the new log immediately.
Scripts are streamed as data and are never sourced or executed by slurm-log.
The legacy singular `sbatchBank` setting is still accepted. A unique script can
be submitted by relative path; if multiple banks contain that path, prefix it
with `BANK/`.

Press `x` to stop only the focused active job. Space-marked jobs are ignored,
so a pane-opening selection cannot accidentally broaden a cancellation. A
collapsed group must be expanded before one of its jobs can be stopped. The
confirmation names the affected job before issuing its owner-authorized
`scancel`.

Environment variables override the file when needed:

- `SLURM_LOG_LOCAL_USER`
- `SLURM_LOG_REMOTE_USER`
- `SLURM_LOG_SSH_HOST`
- `SLURM_LOG_STATE`
- `SLURM_LOG_CONFIG`
- `SLURM_LOG_ARCHIVE_DAYS` (bounded accounting horizon, default 365; range 1–3650)

CLI overrides are also available through `--local-user`, `--remote-user`,
`--ssh-host`, and `--state-path`.

Only the configured owners are passed to SLURM queries. The program does not
enumerate other users' jobs.

## Daemon and SSH behavior

The first query automatically starts a private per-user daemon. It uses a
mode-0600 Unix socket, holds canonical hot metadata snapshots in memory, and
stops after five idle minutes. Stale snapshots are returned immediately while
one background refresh updates every window. MCP log requests add a bounded
64-job/64-MiB in-memory cache; ordinary TUI use does not create a persistent
follower. The daemon never exposes a network port, and remote MCP reads remain
bounded one-shot commands over the existing SSH multiplexing connection.

```bash
slurm-log daemon status
slurm-log daemon start
slurm-log daemon stop
```

Live queue metadata is checked at most once every 15 seconds while a picker
requests updates. All windows share that refresh; filter changes do not create
additional scheduler queries. Accounting history and the archive are cached for
60 seconds. Polling stops shortly after the last active
client, while the daemon itself remains available for five idle minutes. SSH
multiplexing reuses connections for 120 seconds. Pressing `r` requests fresh
data, subject to a hard ten-second per-user rate limit. An early request is
visibly queued and retried automatically.

Job-detail views share a bounded per-user cache. Pending details reuse `squeue`
without an accounting query; active details issue at most one coalesced
job-specific sample every 30 seconds. Refresh phases are staggered, failures back
off exponentially, and terminal snapshots stop polling.

If the daemon is unavailable, clients transparently use the direct query path.

## Updating and uninstalling

Update directly to the latest signed release:

```bash
slurm-log update
```

The compiled updater embeds the reviewed `release-public-key.pem` trust anchor.
It verifies the detached manifest signature before downloading the archive,
checks the signed size and SHA-256 digest before extraction or candidate
execution, atomically replaces the current executable, and preserves
configuration and history. It rejects downgrades by default. If the private
daemon was running, it is restarted with the new binary. An already-downloaded
or locally built binary can be installed without network access:

```bash
slurm-log update --binary ./slurm-log
```

Uninstall while preserving configuration and history:

```bash
slurm-log uninstall
```

Use `slurm-log uninstall --purge` only when the per-user configuration and job
history should also be removed. The standalone `update.sh` and `uninstall.sh`
remain available in release archives for recovery and scripted deployments.

## Building a release archive

From the source directory:

```bash
./package.sh
```

This creates `dist/slurm-log-linux-ARCH.tar.gz`, its `.sha256` file, and a
strict unsigned `.manifest`. The archive contains portable source, installer
scripts, documentation, an optimized native binary, and the example
configuration. It excludes Cargo build trees, personal configuration, state,
sockets, and job history. Packaging intentionally fails while
`release-public-key.pem` is `UNCONFIGURED`.

Pushing a `v*` tag runs the offline suite in GitHub Actions, builds a static
x86-64 Linux binary, and passes only the unsigned artifact set to a protected
OpenSSL signing job. That job validates the tag, archive digest, size, and
canonical manifest before producing a detached `.manifest.sig`. A separate
`contents: write` publisher independently verifies that signature before
creating the GitHub release.

### Release-authentication bootstrap

Before the first signed release, create a dedicated Ed25519 signing key in an
approved key-management system. Never commit, print, or pass its private half
to Cargo, build scripts, procedural macros, or repository executables. Export
only the public half in canonical PEM form and replace `UNCONFIGURED` in
`release-public-key.pem` in a reviewed source commit. The production binary
compiles that PEM into its updater; it does not read a public key from a release
server or runtime environment.

Configure the protected `release-signing` environment with the unencrypted
PKCS#8 PEM secret `SLURM_LOG_RELEASE_SIGNING_KEY_PEM`. Configure the protected
`release-publishing` environment with the matching public PEM secret
`SLURM_LOG_RELEASE_PUBLIC_KEY_PEM`, and require reviewers for both
environments. The signing job copies the reviewed public PEM before its private
secret is injected, compares the derived public key to it, uses OpenSSL only to
sign, verifies the resulting signature, and removes temporary private-key
material before upload. The publisher verifies again with its independently
configured public PEM. Restrict tag creation and environment approval to the
release maintainers.

For a new installation, distribute the public PEM through a pinned reviewed
commit, an authenticated organization channel, or an offline deployment
bundle. Treat a PEM fetched from the same mutable release root as the archive
as untrusted; it defeats the independent trust anchor. Hermetic tests generate
an ephemeral key in a private temporary directory and use the explicitly
compile-time `SLURM_LOG_TEST_BUILD=1` fixture path only; that route is absent
from normal production binaries.

## Manual build

```bash
cargo build --locked --release
install -m 755 target/release/slurm-log ~/.local/bin/slurm-log
```

The compiled binary has no Python dependency. The project is licensed under
the MIT License.

## Testing

The complete suite is offline and does not contact a scheduler or SSH host:

```bash
./test-all.sh
```

It covers every documented CLI view, command, option, picker key, workspace
binding, mouse action, and details-pane control. Process-level tests exercise
direct log opening, exact multi-pane selection, auto-add, session management,
daemon lifecycle and auto-start, canonical/filter/archive caches, concurrent
cold clients, and scheduler failures through fake local commands and private
tmux/socket instances. It also covers arrays, warning/exception filtering,
pending logs, cluster/accounting fallbacks, sbatch discovery/submission/cancel,
setup, MCP negotiation/tools/resources/subscriptions/two-phase mutations,
forty-client daemon sharing, hostile inputs, atomic/private state, installation,
update, uninstall, and package privacy.

`tests/feature_manifest.tsv` maps each command and feature contract to its
owning integration test. `tests/feature_coverage.sh` fails if the documented
surface loses coverage or the matrix unexpectedly shrinks. Everything in
`test-all.sh` is hermetic and offline: it never contacts Slurm, SSH, or a real
tmux workspace. Performance budgets run against optimized release code to
avoid debug-build distortion.

For merged source coverage across unit tests and the process-level integration
suite, install `cargo-llvm-cov` and run:

```bash
cargo install cargo-llvm-cov --locked
./coverage.sh
```

The coverage run builds an instrumented binary in a temporary directory and
uses only fake scheduler/SSH commands. It enforces at least 95% line coverage
across the complete production executable, including the TTY, tmux, setup,
follower, daemon, and process-lifecycle adapters. Override the single gate with
`SLURM_LOG_COVERAGE_MINIMUM`.

Run the online dependency security gate separately:

```bash
cargo install --locked --version 0.22.2 cargo-audit
cargo install --locked --version 0.20.2 cargo-deny
cargo install --locked --version 0.13.0 cargo-geiger
./security-audit.sh
```

It refreshes RustSec advisories, rejects vulnerable or unmaintained crates,
enforces approved licenses and crates.io-only sources, reports duplicate
dependency versions, and verifies that the application forbids unsafe Rust.
