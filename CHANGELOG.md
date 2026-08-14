# Changelog

## 0.3.0

- Add twelve MCP tools that keep a run inside MCP end to end: `slurm_doctor`,
  `slurm_refresh_bank`, `slurm_wait_job`, `slurm_explain_pending`,
  `slurm_find_artifact`, `slurm_read_declared_result`, `slurm_stage_bundle`,
  `slurm_adopt_job`, `slurm_preflight_job`, `slurm_preview_resubmit`, plus a
  digest-bound `slurm_submit_job` response and provenance metadata on script
  search results.
- Text fallbacks now include result counts, warnings, cursor, and first
  matches, so clients no longer see a bare "structured result attached".
- `slurm_doctor` separates scheduler reachability from bank health and reports
  catalog generation, indexed script count, and last refresh.
- Safe declared-result reading bound to `#SLURM_LOG-RESULT:` basename globs
  declared by the batch script, confined beneath the configured working
  directory and MIME-sniffed and size-limited.
- Content-addressed bundle staging with an explicit repo-relative manifest,
  size and secret-path checks, an immutable remote destination, and separate
  execution approval.
- Scheduling-only resubmission preview (`--partition`, `--dependency`, etc.)
  that binds the unchanged producer hash and never auto-switches partitions.
- Bounded server-side job waiting for state, completion, and log changes, and
  pending-job explanation with partition availability.
- Evidence-backed diagnosis separating cancellation, environment, OOM, timeout,
  and application failures from scheduler errors.

## 0.2.6

- Construct a `--clusters` scheduler argument only when a cluster configures
  an explicit controller, restoring remote single-cluster behaviour: a remote
  scheduler no longer receives a fabricated `--clusters <label>` that it
  rejects as an unknown cluster.
- Validate returned submission controllers only when one is configured, and
  let routing directives stand when no controller identity is declared.

## 0.2.5

- Treat local cluster names as display labels unless an explicit controller is
  configured, preventing invalid `--clusters` arguments for local schedulers.
- Flatten multi-line scheduler failures and terminal control bytes before
  rendering picker footer warnings, preserving the one-row UI invariant.

## 0.2.4

- Bind remote scheduler operations and mutation results to the configured
  controller, and reject array-wide cancellation through bare master IDs.
- Enforce fresh owner/controller authorization and descriptor-confined log
  and bank access across MCP, daemon, details, and subscription paths.
- Bound MCP frames, concurrent blocking work, command lifetimes, release
  downloads, and archive extraction.
- Authenticate releases with a signed canonical manifest and an immutable
  Ed25519 public key before downloading, extracting, or executing artifacts.
- Split verification, signing, and publishing into separately privileged
  GitHub jobs with protected environment secrets and pinned tooling.

## 0.2.3

- Make long-lived MCP connections discover newly configured sbatch banks and
  added, removed, or changed scripts inside existing nested bank directories.
- Include script and warning counts in the `slurm_list_scripts` text fallback.

## 0.2.2

- Keep verbose picker warnings on one terminal row so they cannot auto-wrap
  the footer and scroll the header off-screen.
- Preserve the actionable blocked-job status ahead of truncated warning text.

## 0.2.1

- Keep completed jobs dismissed on the first attempt by reloading the current
  private state ledger for daemon replies instead of replaying cached state.

## 0.2.0

- Add the client-neutral `slurm-log mcp` stdio server with structured tools,
  resources, templates, subscriptions, and guided client setup.
- Add bounded, generation-aware job-log reads, search, deterministic diagnosis,
  and a shared private-daemon LRU cache.
- Add digest-bound two-phase bank submission, exact name-bound cancellation,
  and private rotating MCP mutation audit records.
- Preserve the existing configuration format, CLI/TUI behavior, daemon wire
  request ordering, caches, lifecycle flow, and installation layout.

## 0.1.8

- Previous CLI/TUI release.
