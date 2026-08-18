# Changelog

## 0.4.0

- Add an interactive action toast system in the picker interface with 1.5-second
  expiring notices for selection toggles (`Space`), bulk selection (`v`), clearing (`c`),
  cluster cycling (`Tab` / `Shift-Tab`), group expanding/collapsing (`→` / `←`),
  scheduler refresh (`r`), search clearing (`Esc`), and auto-add toggles (`A`).
- Implement adaptive keylines in the picker header that dynamically scale with terminal
  width to progressively surface `i details`, `s submit`, `d dismiss`, and navigation controls.
- Add multi-selection action hints to the footer (`(Enter open · c clear)`) and tmux
  workspace shortcut hints to interactive allocation frames (`Ctrl-b i details · Ctrl-b z zoom`).
- Preserve carriage returns (`\r`) and raw terminal ANSI escape sequences in `filter_log`,
  preventing progress bars (`tqdm`, Hugging Face, PyTorch, spinners) from printing
  repeated newlines and staircasing in followed log panes.
- Persist the `[W WARN ON / OFF]` log warning toggle across panel transitions and workspace
  sessions via the tracking ledger on disk.
- Classify interactive interpreters and debuggers (`python`, `ipython`, `julia`, `node`, `R`,
  `gdb`, `cuda-gdb`, `matlab`, etc.) across arbitrary virtualenv, conda, system, and custom
  paths as interactive, placing them in the blocked listing (`b` toggle).
- Distinguish unschedulable dead dependencies (`DependencyNeverSatisfied`) from actionable
  pending jobs (`Priority`, `Resources`, `Dependency`, `QOS`, `ReqNodeNotAvail`, `BeginTime`),
  keeping actionable jobs immediately visible in the default live queue.
- Optimize tmux workspace lifecycle and reconcile operations with batched commands and
  zero-flicker cursor-home in-place rendering.

## 0.3.2
- Fix Slurm job array task identity validation when `scontrol` returns the master
  JobId alongside `ArrayJobId` and `ArrayTaskId`, resolving "scontrol response job ID
  does not match" errors on sub-jobs.
- Classify non-shell interactive allocations (Python, IPython, Julia, Node, R,
  debuggers, etc.) as interactive, correctly placing them in the blocked listing.
- Switch tmux workspace `set-clipboard` to `on` so mouse/copy-mode selections are
  stored in tmux paste buffers (`prefix + ]`) in addition to OSC-52 forwarding.

## 0.3.1

- Add live inline resource usage sparklines (CPU, Memory, GPU) to `Ctrl-b i`
  compact pane and full `slurm-log details` views.
- Compute interval delta CPU efficiency between poll samples for responsive
  instantaneous activity metrics.
- Make `--clusters` argument injection strictly opt-in via explicit controller
  configuration, preventing multi-cluster database errors on standalone and
  differently named local/remote clusters.

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
