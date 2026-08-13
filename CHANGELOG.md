# Changelog

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
