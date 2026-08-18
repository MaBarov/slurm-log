# Slurm-Log Performance Optimization Plan
*Date: 2026-08-17*

## Executive Summary
Comprehensive subsystem profiling across log streaming, Slurm queue parsing, `.sbatch` script bank indexing, terminal UI rendering, and state management established exact latency and throughput baselines. This plan outlines phased optimizations targeting memory allocation, rendering latency, and parsing throughput.

---

## Baseline Performance Metrics

| Subsystem / Operation | Workload | Latency | Throughput |
| :--- | :--- | :--- | :--- |
| **Log Sanitization** (`sanitize`) | 4 MiB buffer (~32k lines) | **4.49 ms** | 890.9 MiB/s |
| **Log Search (Literal)** | 4 MiB buffer, 100 matches + ctx | **1.63 ms** | 613 queries/s |
| **Log Search (Regex)** | 4 MiB buffer, regex match | **1.04 ms** | 961 queries/s |
| **Queue Parser** (`parse_queue`) | 50,000 jobs (`squeue`) | **33.68 ms** | 1.48M jobs/s |
| **Control Stripper** (`terminal_text`) | Single clean field | **43.00 ns** | 23.2M calls/s |
| **Sbatch Directives Parser** | Single `.sbatch` script | **613.00 ns** | 1.63M scripts/s |
| **Script SHA256 Hashing** | Single `.sbatch` script | **399.00 ns** | 2.51M hashes/s |
| **Sparklines Rendering** | 30-slot padded ring buffer | **374.00 ns** | 2.67M renders/s |
| **ANSI Truncator** (`truncate_ansi`) | 50-col formatted row | **105.00 ns** | 9.52M rows/s |
| **State Ledger Serialization** | 5,000 tracked job IDs | **91.80 µs** | 58.7 KB JSON |
| **State Ledger Deserialization** | 5,000 tracked job IDs | **888.80 µs** | ~1,125 reads/s |

---

## Phased Implementation Plan

### Phase 1: High Impact (Memory & Allocation Overhead)
1. **Fast-Path ASCII in `truncate_ansi` & UI Table String Layout**:
   - Add fast-path byte slicing for non-escape, non-Unicode strings to eliminate per-character `UnicodeWidthChar` traversal in table rendering.
2. **State Ledger In-Memory Deduplication & Atomic Flushes**:
   - Optimize `state.json` read/write serialization, avoiding redundant set allocations on UI events.

### Phase 2: Medium Impact (Throughput & Rendering)
3. **Integer / LUT-Based Sparkline Mapping**:
   - Replace floating point normalization in `spark_padded` with integer arithmetic or pre-computed lookup tables for the 8 spark characters (` ·▂▃▄▅▆▇█`).
4. **Sbatch Directives Tokenizer Optimization**:
   - Optimize `#SBATCH` token extraction to avoid intermediate string allocations during directory scanning.

### Phase 3: Micro-Optimizations (Sub-Microsecond Hot Paths)
5. **64-bit Chunk Word Scanning for Log Sanitizer**:
   - Process 8 bytes per iteration using bitmask checks for printable ASCII to boost sanitization throughput past 1.5 GiB/s.

---

## Verification & Benchmarking
- Measure each phase with `benches/profile_full.rs`.
- Verify full unit test suite: `cargo test --manifest-path Cargo.toml`.
- Rebuild release binary and verify daemon restart.
