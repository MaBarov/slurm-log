# slurm-log: In-Pane Resource Usage Sparklines & Metric Precision Plan

**Date**: 2026-08-17  
**Component**: `slurm-log` (`src/details/`)  
**Target View**: `Ctrl-b i` (Compact details pane) & `slurm-log details <JOB_ID>` (Full view)

---

## 1. Problem Statement & Motivation

1. **Metric Semantics (Cumulative vs. Live)**:
   - **CPU**: Slurm (`sstat`/`sacct`) returns cumulative `TotalCPU` ($U + S$ time). `cpu_efficiency = TotalCPU / (Elapsed * CPUs)` is a running lifetime average. It responds sluggishly to recent stalls or bursts.
   - **Memory**: Slurm returns `MaxRSS` (peak resident set size observed across all tasks). `memory_efficiency = MaxRSS / AllocMem` is strictly monotonic (peak, not instantaneous working set).
   - **GPU**: Extracted from TRES `gres/gpuutil` and `gres/gpumem`. If accounting counters exist, it reflects the latest sample / cluster interval average.

2. **Visual Feedback in Compact Mode (`Ctrl-b i`)**:
   - `Ctrl-b i` runs with `SLURM_LOG_DETAILS_COMPACT=1` in a 38% tmux split pane (~8–10 lines high).
   - The compact view currently displays flat percentages without trends.
   - The full dashboard (`compact = false`) tracks `cpu` and `memory` ring buffers (`VecDeque<f64>`) with sparklines (`spark()`), but omits `gpu` and is hidden in compact mode.

---

## 2. Design Specification

### 2.1 Compact Pane Layout (`Ctrl-b i`)
Replace the static `USAGE` row with inline sparklines bounded to fixed character widths (e.g. 8 bars), keeping total pane height strictly under 7–8 lines:

```text
DETAILS  sprint1:461161  train_transformer
RUNNING  ·  elapsed 00:42:15 / 04:00:00
ALLOC    1 node · 16 CPU · 1 GPU · 64.0 GiB memory
USAGE    CPU 84.2% [ ▂▃▅▆▇██] · Mem 42.1% [▅▅▅▅▅▅▅▅] · GPU 99.0% [████████]
PLACE    all · sprint1
NOTE     Peak memory is close to allocated limit (85%)
14:22:05 · sstat · live · auto 30s
Ctrl-b i / q / Esc close · Space pause · r refresh
```

### 2.2 Dedicated Dashboard Layout (`slurm-log details <ID>`)
In full view (`compact = false`), provide full-width dedicated trend rows including GPU:

```text
UTILIZATION
  CPU            84.2% (cumulative)  /  98.1% (live interval)
  Memory         42.1% (26.9 GiB peak of 64.0 GiB allocated)
  GPU            99.0% (NVIDIA A40, 44.8 GiB / 48.0 GiB VRAM)
  CPU trend       ▂▃▅▆▇██▇▆▅▄▃▂  ▂▃▄▅▆▇██
  Memory trend   ▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅▅
  GPU trend      ████████████████████████
```

---

## 3. Mathematical Specifications

### 3.1 Interval (Delta) CPU Calculation
To represent real-time activity rather than an over-smoothed lifetime average:
$$\text{Interval CPU \%} = \frac{\text{TotalCPU}(t_k) - \text{TotalCPU}(t_{k-1})}{(t_k - t_{k-1}) \times N_{\text{CPUs}}} \times 100$$
- Clamp to `[0.0, 100.0]`.
- First sample fallback: use `cpu_efficiency` (lifetime average).

### 3.2 Peak Memory Efficiency
$$\text{Memory Peak \%} = \frac{\text{MaxRSS}}{\text{Allocated Memory}} \times 100$$

### 3.3 GPU Utilization
$$\text{GPU \%} = \text{gres/gpuutil} \in [0.0, 100.0]$$

---

## 4. Implementation Steps in Codebase

### Step 1: Update `src/details/control.rs`
- Add `gpu: VecDeque<f64>` ring buffer (capacity 40).
- Add `prev_cpu_time: Option<(Instant, u64)>` tracking.
- Compute interval delta for CPU samples.
- Record `gpu_utilization` samples when present.
- Pass `&gpu` to `draw()`.

### Step 2: Update `src/details/render.rs`
- Implement `spark_inline(values: &VecDeque<f64>, max_width: usize) -> String` using Unicode blocks `[' ', '▂', '▃', '▄', '▅', '▆', '▇', '█']`.
- Update compact `draw()` to format the `USAGE` line with inline sparklines.
- Dynamically scale sparkline width if terminal columns < 80 to prevent line-wrapping.
- Handle missing GPU accounting (`none allocated` vs `not recorded`) gracefully without empty brackets.

### Step 3: Update `src/details/tests.rs` & Integration Tests
- Unit test `spark_inline` for empty, single-sample, full, and overflow states.
- Unit test delta CPU computation across clock increments.
- Verify renderer outputs for compact mode with and without GPU accounting.
- Run `cargo test` and coverage gates.

---

## 5. Edge Cases & Safeguards

1. **Cold Start (1st sample upon opening `Ctrl-b i`)**:
   - Renders single bar `[█]` or placeholder `[·]`, growing dynamically as 30s auto-refresh ticks occur.
2. **Missing GPU Plugin / Unrecorded Clusters**:
   - If `gpus == 0` $\rightarrow$ `GPU none allocated` (no sparkline).
   - If `gpus > 0` but `gpu_utilization.is_none()` $\rightarrow$ `GPU not recorded` (no sparkline brackets, saving width).
3. **Job Completion / Terminal Freeze**:
   - When `details.terminal == true`, polling halts and sparkline history freezes on the final state.
4. **Terminal Resizing**:
   - `Event::Resize` redraws using clamped width proportional to terminal size.
