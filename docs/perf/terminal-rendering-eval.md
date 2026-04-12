# Terminal Rendering Performance Evaluation

## Current Architecture

ZRemote's terminal rendering uses a two-level cache architecture in `terminal_element.rs`:

### Per-character Glyph Cache (`GlyphCache`)

- **Key**: `(char, bold, italic, wide, color_h, color_s, color_l, color_a)`
- **Storage**: `HashMap<GlyphCacheKey, ShapedLine>`, pre-allocated to 512 entries
- **Eviction**: None needed -- bounded by character set x style x color combinations
- **Purpose**: Eliminates per-frame `shape_line()` calls (each ~10ms) by caching individual character shapes

In a monospace terminal, each character+style+color combination always produces the same glyph. Instead of shaping entire text runs (where "Hello" and "hello" are different cache keys), individual characters are shaped once and reused. This bounds the cache to approximately `chars x styles x colors` entries (~500 in practice).

### Cell Run Cache (`CellRunCache`)

- **Key**: `(display_offset, content_generation)`
- **Storage**: 8-slot LRU ring buffer of `Vec<CellRun>`
- **Eviction**: Oldest entry when at capacity
- **Purpose**: Avoids rebuilding cell runs when scrolling back to recently visited positions

Cell runs batch adjacent terminal cells with identical styles (fg, bg, bold, italic, etc.) into contiguous groups. The cache stores the complete `Vec<CellRun>` for each unique viewport state.

### Rendering Pipeline

```
PTY output -> VTE processor -> alacritty_terminal grid
                                      |
                                      v
                             build_cell_runs() -> Vec<CellRun>
                                      |
                                      v
                             GlyphCache lookups -> shaped glyphs
                                      |
                                      v
                             GPUI paint calls (backgrounds, text, cursor)
```

## Methodology

Benchmarks measure the **data processing pipeline** up to where GPUI paint calls would occur. GPUI rendering requires a window context unavailable in headless tests, so we instrument:

1. **VTE processing**: Time to feed raw ANSI bytes through alacritty_terminal's VTE parser
2. **Cell run construction**: Time to scan the terminal grid and build styled runs
3. **Cache lookup performance**: HashMap lookup latency for both caches
4. **End-to-end pipeline**: Simulated real-world scenarios combining VTE + cell runs + cache

All measurements taken on the development machine (Linux 6.19.10), Rust 1.94.0, optimized test profile.

## Baseline Numbers

### VTE Processing

| Scenario | Content Size | Time | Throughput |
|---|---|---|---|
| ANSI log (10K lines, mixed colors/styles) | 1,253 KB | 30.3ms | 42.3 MB/s |
| TUI redraws (100 full-screen frames) | 794 KB | 18.3ms | 44.3 MB/s |
| Large paste (50K lines, mostly plain) | 4,100 KB | 81.7ms | 52.3 MB/s |

VTE processing is handled entirely by alacritty_terminal and is not a bottleneck for typical terminal usage.

### Cell Run Construction (`build_cell_runs`)

| Scenario | Terminal Size | Time | Runs/Frame | Cells/Run |
|---|---|---|---|---|
| ANSI log (many colors) | 120x40 | 161us | 181 | 26.5 |
| TUI layout (tables, borders) | 120x40 | 110us | 134 | 35.8 |
| Plain text (minimal styling) | 120x40 | 102us | 40 | 120.0 |
| Large terminal (ANSI log) | 240x80 | 532us | 451 | 42.6 |

**Frame budget impact**: At 60fps (16.6ms per frame), `build_cell_runs` consumes:
- 120x40 grid: 0.6--1.0% of frame budget
- 240x80 grid: 3.2% of frame budget

### Glyph Cache Performance

| Metric | Value |
|---|---|
| Unique (char, style) combos per frame (ignoring color) | ~65 |
| Total non-space chars per frame (120x40) | ~1,817 |
| Steady-state lookup latency | 6ns per char |
| Total lookup time per frame | ~11us (1,817 lookups) |
| First-frame hit rate | 0% (all misses, shaping required) |
| Subsequent frames hit rate | ~100% (all HashMap hits) |

The glyph cache is extremely effective. After the first frame populates it, all subsequent frames perform only HashMap lookups at ~6ns each, completely eliminating the ~10ms-per-miss `shape_line()` cost.

### Cell Run Cache Performance

| Scenario | Hit Rate | Notes |
|---|---|---|
| Static content (no PTY output) | 99.9% | Same offset + same generation = hit |
| tail -f (continuous output) | 0% | Every frame has new content_generation |
| Scrollback (back-and-forth) | Varies | Effective when content is static during scroll |
| Full-screen TUI updates | 0% | Every frame is a full redraw with new generation |

**Key insight**: The cell run cache provides value only during scrollback through static content. For actively updating terminals (tail -f, TUI apps), every frame is a cache miss because `content_generation` increments with each PTY output batch. This is by design -- the cache exists specifically for the scrollback use case.

### Memory Usage

| Component | Size |
|---|---|
| Cell runs per frame (120x40, ANSI) | 27.2 KB (17 KB structs + 10 KB text) |
| CellRunCache (8 slots, worst case) | ~217 KB |
| GlyphCache (~500 entries, estimated) | ~109 KB |
| **Total rendering overhead** | **~350 KB** |

### End-to-End Pipeline Scenarios

#### tail -f at 500 lines/second (5-second simulation)

| Metric | Value |
|---|---|
| Frames simulated | 300 (60fps) |
| Per-frame average | 158us |
| Cell run build time (avg per miss) | 112us |
| Frame budget usage | ~1.0% |

#### Full-screen TUI at 30fps (5-second simulation)

| Metric | Value |
|---|---|
| Frames simulated | 150 |
| Per-frame average | 411us |
| Cell run build time (avg) | 131us |
| VTE processing per frame | ~280us |
| Frame budget usage | ~2.5% |

#### Large paste (50K lines, 4.1 MB)

| Metric | Value |
|---|---|
| Total processing time | 89ms |
| VTE processing | 83ms (93%) |
| Cell run builds (65 frames) | 7ms (7%) |
| Effective throughput | 47.9 MB/s |

## Analysis

### What Works Well

1. **Per-character glyph cache is near-optimal.** With ~65 unique character+style combinations per frame and 6ns lookups, the total glyph cache overhead is ~11us/frame -- negligible. The key insight of caching individual characters instead of text runs makes the cache both bounded and highly effective.

2. **Cell run construction is fast.** At 100--160us for a standard 120x40 terminal, it uses less than 1% of the 60fps frame budget. Even a 240x80 terminal only uses 3.2%.

3. **Memory footprint is modest.** ~350 KB total for all rendering caches and per-frame data. No concern for memory pressure.

4. **VTE processing throughput is excellent.** 42--52 MB/s means even a 1 MB paste processes in ~20ms. The `opt-level = 3` override for `alacritty_terminal` in dev profiles helps significantly.

### Where the Cell Run Cache Is Less Effective

The cell run cache has 0% hit rate for the two most common terminal usage patterns:
- **tail -f / streaming output**: `content_generation` increments every 16ms (coalescing timer)
- **TUI applications**: Every frame is a full screen redraw

This is not a problem in practice because `build_cell_runs` is fast enough (~130us) that rebuilding every frame stays well within budget. The cache only adds value during scrollback through history, which is its intended purpose.

### Potential Improvement: Dirty Line Tracking

The most impactful optimization would be tracking which grid lines changed between frames, allowing incremental updates to the cell run vector instead of a full rebuild. In the `tail -f` scenario, typically only the bottom 8 lines change per frame (at 500 lines/s, 60fps), but we currently rebuild all 40 rows.

**Estimated savings**: For tail -f with 8/40 dirty lines, incremental builds could reduce cell run construction from ~130us to ~26us (5x improvement). However, the current 130us is already well within budget, so this is low priority.

### Potential Improvement: CellRun String Pre-allocation

The text field in `CellRun` pre-allocates 40 bytes per run, but actual utilization is 46%. Reducing to 20 bytes for short runs or using a small-string optimization could save ~5 KB per frame. Again, not a meaningful win given the current ~27 KB total.

## Recommendations

**Current approach is optimal for the workload.** The dual-cache architecture is well-designed:

1. **No changes needed.** The rendering pipeline uses less than 3% of frame budget even in worst-case scenarios (large terminal + complex content).

2. **Keep the cell run cache.** Despite 0% hit rate for streaming content, it provides real value during scrollback browsing and costs only 217 KB of memory.

3. **Low-priority future work**: If terminal sizes grow significantly (e.g., 4K display at small font size producing 300x80+ grids), dirty line tracking would be the first optimization to implement.

4. **Monitor with these benchmarks.** Run `cargo test --package zremote-gui terminal_bench -- --nocapture --ignored` periodically to detect regressions.

## Test Scenarios Covered

| Scenario | What It Tests | Budget Impact |
|---|---|---|
| tail -f (500 lines/s) | Streaming log output with ANSI colors | 1.0% |
| Full-screen TUI (30fps) | htop-like full redraws with tables/borders | 2.5% |
| Large paste (50K lines) | Bulk text insertion throughput | 89ms total |
| Scrollback browsing | Cache effectiveness for back-and-forth scrolling | ~0% (cached) |
| Large terminal (240x80) | Scaling with terminal size | 3.2% |

## How to Run

```bash
# All benchmarks (summary)
cargo test --package zremote-gui terminal_bench::tests::bench_summary -- --nocapture --ignored

# Individual benchmark groups
cargo test --package zremote-gui terminal_bench::tests::bench_vte -- --nocapture --ignored
cargo test --package zremote-gui terminal_bench::tests::bench_cell_runs -- --nocapture --ignored
cargo test --package zremote-gui terminal_bench::tests::bench_cell_run_cache -- --nocapture --ignored
cargo test --package zremote-gui terminal_bench::tests::bench_glyph -- --nocapture --ignored
cargo test --package zremote-gui terminal_bench::tests::bench_pipeline -- --nocapture --ignored
cargo test --package zremote-gui terminal_bench::tests::bench_memory -- --nocapture --ignored
```
