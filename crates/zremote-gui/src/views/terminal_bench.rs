//! Terminal rendering performance benchmarks.
//!
//! Measures the data processing pipeline up to the point where GPUI elements
//! would be created. Since GPUI rendering requires a window context that cannot
//! be created in headless tests, we focus on:
//!
//! - VTE processing: feeding raw ANSI bytes into alacritty_terminal
//! - Cell run construction: `build_cell_runs()` converting grid to styled runs
//! - Cache behavior: hit/miss rates for `CellRunCache` and `GlyphCache`
//!
//! Run with: `cargo test --package zremote-gui terminal_bench -- --nocapture --ignored`

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use alacritty_terminal::term::Config as TermConfig;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use crate::views::terminal_element::{
        CellRun, CellRunCache, CellRunCacheEntry, GlyphCache, GlyphCacheKey, TerminalElement,
    };

    /// No-op event listener for benchmark terminals.
    #[derive(Clone)]
    struct NoopListener;

    impl alacritty_terminal::event::EventListener for NoopListener {
        fn send_event(&self, _event: alacritty_terminal::event::Event) {}
    }

    /// Create a terminal with given dimensions and feed it raw bytes.
    fn create_term_with_content(
        cols: usize,
        rows: usize,
        content: &[u8],
    ) -> alacritty_terminal::Term<NoopListener> {
        let config = TermConfig::default();
        let size = TermSize::new(cols, rows);
        let mut term = alacritty_terminal::Term::new(config, &size, NoopListener);
        let mut processor: Processor<StdSyncHandler> = Processor::new();
        processor.advance(&mut term, content);
        term
    }

    /// Generate synthetic ANSI log content (~10K lines of mixed styles/colors).
    fn generate_ansi_log(line_count: usize) -> Vec<u8> {
        let mut buf = Vec::with_capacity(line_count * 120);
        let colors_16 = [31, 32, 33, 34, 35, 36, 91, 92, 93, 94, 95, 96];
        let styles = ["1", "2", "3", "4", "7", "9"]; // bold, dim, italic, underline, reverse, strikethrough

        for i in 0..line_count {
            // Timestamp (dim)
            buf.extend_from_slice(b"\x1b[2m2025-01-15T12:00:00.000Z\x1b[0m ");

            // Log level with color
            let color = colors_16[i % colors_16.len()];
            buf.extend_from_slice(format!("\x1b[1;{color}m").as_bytes());
            match i % 5 {
                0 => buf.extend_from_slice(b" INFO"),
                1 => buf.extend_from_slice(b" WARN"),
                2 => buf.extend_from_slice(b"ERROR"),
                3 => buf.extend_from_slice(b"DEBUG"),
                _ => buf.extend_from_slice(b"TRACE"),
            }
            buf.extend_from_slice(b"\x1b[0m ");

            // Path with 256-color
            let path_color = 16 + (i % 216);
            buf.extend_from_slice(format!("\x1b[38;5;{path_color}m").as_bytes());
            buf.extend_from_slice(b"src/views/terminal.rs\x1b[0m: ");

            // Mix in different style combinations
            if i % 7 == 0 {
                let style = styles[i % styles.len()];
                buf.extend_from_slice(format!("\x1b[{style}m").as_bytes());
                buf.extend_from_slice(b"styled message text with emphasis");
                buf.extend_from_slice(b"\x1b[0m");
            } else if i % 11 == 0 {
                // True color
                let r = (i * 3) % 256;
                let g = (i * 7) % 256;
                let b = (i * 13) % 256;
                buf.extend_from_slice(
                    format!("\x1b[38;2;{r};{g};{b}mTrue color message\x1b[0m").as_bytes(),
                );
            } else {
                buf.extend_from_slice(b"Request processed successfully in 12ms");
            }

            // Occasional Unicode
            if i % 15 == 0 {
                buf.extend_from_slice(" \u{2500}\u{2500}\u{2500} \u{2714} OK".as_bytes());
            }

            buf.push(b'\n');
        }
        buf
    }

    /// Generate TUI-like full-screen redraws (box drawing, tables, colors).
    fn generate_tui_frames(frame_count: usize, cols: usize, rows: usize) -> Vec<u8> {
        let mut buf = Vec::with_capacity(frame_count * cols * rows * 3);

        for frame in 0..frame_count {
            // Clear screen + home
            buf.extend_from_slice(b"\x1b[2J\x1b[H");

            // Header bar (reverse video)
            buf.extend_from_slice(b"\x1b[7m");
            let header = format!(" MONITOR v1.0 - Frame {frame:04} ");
            buf.extend_from_slice(header.as_bytes());
            let pad = cols.saturating_sub(header.len());
            buf.extend(std::iter::repeat_n(b' ', pad));
            buf.extend_from_slice(b"\x1b[0m\n");

            // Box drawing border
            buf.extend_from_slice("\x1b[38;5;240m\u{250c}".as_bytes());
            for _ in 0..cols.saturating_sub(2) {
                buf.extend_from_slice("\u{2500}".as_bytes());
            }
            buf.extend_from_slice("\u{2510}\x1b[0m\n".as_bytes());

            // Table rows with alternating backgrounds
            for row in 0..rows.saturating_sub(4) {
                let bg = if row % 2 == 0 {
                    "\x1b[48;5;235m"
                } else {
                    "\x1b[48;5;233m"
                };
                buf.extend_from_slice(bg.as_bytes());
                buf.extend_from_slice("\x1b[38;5;240m\u{2502}\x1b[0m".as_bytes());
                buf.extend_from_slice(bg.as_bytes());

                // PID column (bold)
                let pid = (frame * rows + row) % 32768;
                buf.extend_from_slice(format!("\x1b[1m{pid:>7}\x1b[22m ").as_bytes());

                // CPU% with color gradient
                let cpu = ((frame + row) * 7) % 100;
                let cpu_color = if cpu > 75 {
                    196
                } else if cpu > 25 {
                    226
                } else {
                    46
                };
                buf.extend_from_slice(format!("\x1b[38;5;{cpu_color}m{cpu:>5}%\x1b[0m").as_bytes());
                buf.extend_from_slice(bg.as_bytes());

                // Fill remaining columns
                let used = 7 + 1 + 6;
                let remaining = cols.saturating_sub(used + 2);
                let cmd = " /usr/bin/process --arg value";
                let cmd_bytes = cmd.as_bytes();
                let to_write = remaining.min(cmd_bytes.len());
                buf.extend_from_slice(&cmd_bytes[..to_write]);
                buf.extend(std::iter::repeat_n(
                    b' ',
                    remaining.saturating_sub(to_write),
                ));

                buf.extend_from_slice(b"\x1b[0m\n");
            }

            // Bottom border
            buf.extend_from_slice("\x1b[38;5;240m\u{2514}".as_bytes());
            for _ in 0..cols.saturating_sub(2) {
                buf.extend_from_slice("\u{2500}".as_bytes());
            }
            buf.extend_from_slice("\u{2518}\x1b[0m\n".as_bytes());

            // Status bar
            buf.extend_from_slice(b"\x1b[7m F1:Help  F5:Refresh  F10:Quit ");
            let status_pad = cols.saturating_sub(31);
            buf.extend(std::iter::repeat_n(b' ', status_pad));
            buf.extend_from_slice(b"\x1b[0m");
        }
        buf
    }

    /// Generate a large paste (50K lines of plain text with occasional colors).
    fn generate_large_paste(line_count: usize) -> Vec<u8> {
        let mut buf = Vec::with_capacity(line_count * 80);
        for i in 0..line_count {
            if i % 100 == 0 {
                // Occasional colored line
                buf.extend_from_slice(format!("\x1b[33m--- section {i} ---\x1b[0m\n").as_bytes());
            } else {
                buf.extend_from_slice(
                    format!(
                        "Line {i:06}: The quick brown fox jumps over the lazy dog. Lorem ipsum dolor sit amet.\n"
                    )
                    .as_bytes(),
                );
            }
        }
        buf
    }

    // ── VTE Processing Benchmarks ─────────────────────────────────────

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_vte_ansi_log_10k_lines() {
        let content = generate_ansi_log(10_000);
        let content_size = content.len();

        // Warm up
        let _ = create_term_with_content(120, 40, &content);

        let iterations = 20u32;
        let start = Instant::now();
        for _ in 0..iterations {
            let _ = create_term_with_content(120, 40, &content);
        }
        let elapsed = start.elapsed();
        let per_iter = elapsed / iterations;
        let throughput_mb = (content_size as f64 / 1_000_000.0) / per_iter.as_secs_f64();

        println!("=== VTE Processing: ANSI Log (10K lines) ===");
        println!("  Content size: {:.1} KB", content_size as f64 / 1024.0);
        println!("  Iterations:   {iterations}");
        println!("  Total time:   {elapsed:?}");
        println!("  Per iteration: {per_iter:?}");
        println!("  Throughput:   {throughput_mb:.1} MB/s");
    }

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_vte_tui_100_frames() {
        let content = generate_tui_frames(100, 120, 40);
        let content_size = content.len();

        let _ = create_term_with_content(120, 40, &content);

        let iterations = 20u32;
        let start = Instant::now();
        for _ in 0..iterations {
            let _ = create_term_with_content(120, 40, &content);
        }
        let elapsed = start.elapsed();
        let per_iter = elapsed / iterations;
        let throughput_mb = (content_size as f64 / 1_000_000.0) / per_iter.as_secs_f64();

        println!("=== VTE Processing: TUI Frames (100 redraws) ===");
        println!("  Content size: {:.1} KB", content_size as f64 / 1024.0);
        println!("  Iterations:   {iterations}");
        println!("  Total time:   {elapsed:?}");
        println!("  Per iteration: {per_iter:?}");
        println!("  Throughput:   {throughput_mb:.1} MB/s");
    }

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_vte_large_paste_50k_lines() {
        let content = generate_large_paste(50_000);
        let content_size = content.len();

        let _ = create_term_with_content(120, 40, &content);

        let iterations = 10u32;
        let start = Instant::now();
        for _ in 0..iterations {
            let _ = create_term_with_content(120, 40, &content);
        }
        let elapsed = start.elapsed();
        let per_iter = elapsed / iterations;
        let throughput_mb = (content_size as f64 / 1_000_000.0) / per_iter.as_secs_f64();

        println!("=== VTE Processing: Large Paste (50K lines) ===");
        println!(
            "  Content size: {:.1} MB",
            content_size as f64 / 1_048_576.0
        );
        println!("  Iterations:   {iterations}");
        println!("  Total time:   {elapsed:?}");
        println!("  Per iteration: {per_iter:?}");
        println!("  Throughput:   {throughput_mb:.1} MB/s");
    }

    // ── Cell Run Construction Benchmarks ──────────────────────────────

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_cell_runs_ansi_log() {
        let content = generate_ansi_log(10_000);
        let term = create_term_with_content(120, 40, &content);

        // Warm up
        let _ = TerminalElement::build_cell_runs(&term);

        let iterations = 500u32;
        let start = Instant::now();
        let mut total_runs = 0;
        for _ in 0..iterations {
            let runs = TerminalElement::build_cell_runs(&term);
            total_runs += runs.len();
        }
        let elapsed = start.elapsed();
        let per_iter = elapsed / iterations;
        let avg_runs = total_runs / iterations as usize;

        println!("=== Cell Run Construction: ANSI Log ===");
        println!("  Terminal size: 120x40");
        println!("  Iterations:    {iterations}");
        println!("  Total time:    {elapsed:?}");
        println!("  Per iteration: {per_iter:?}");
        println!("  Avg runs/frame: {avg_runs}");
        println!("  Avg cells/run: {:.1}", (120.0 * 40.0) / avg_runs as f64);
    }

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_cell_runs_tui() {
        let content = generate_tui_frames(100, 120, 40);
        let term = create_term_with_content(120, 40, &content);

        let _ = TerminalElement::build_cell_runs(&term);

        let iterations = 500u32;
        let start = Instant::now();
        let mut total_runs = 0;
        for _ in 0..iterations {
            let runs = TerminalElement::build_cell_runs(&term);
            total_runs += runs.len();
        }
        let elapsed = start.elapsed();
        let per_iter = elapsed / iterations;
        let avg_runs = total_runs / iterations as usize;

        println!("=== Cell Run Construction: TUI (last frame) ===");
        println!("  Terminal size: 120x40");
        println!("  Iterations:    {iterations}");
        println!("  Total time:    {elapsed:?}");
        println!("  Per iteration: {per_iter:?}");
        println!("  Avg runs/frame: {avg_runs}");
        println!("  Avg cells/run: {:.1}", (120.0 * 40.0) / avg_runs as f64);
    }

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_cell_runs_plain_text() {
        let content = generate_large_paste(50_000);
        let term = create_term_with_content(120, 40, &content);

        let _ = TerminalElement::build_cell_runs(&term);

        let iterations = 500u32;
        let start = Instant::now();
        let mut total_runs = 0;
        for _ in 0..iterations {
            let runs = TerminalElement::build_cell_runs(&term);
            total_runs += runs.len();
        }
        let elapsed = start.elapsed();
        let per_iter = elapsed / iterations;
        let avg_runs = total_runs / iterations as usize;

        println!("=== Cell Run Construction: Plain Text ===");
        println!("  Terminal size: 120x40");
        println!("  Iterations:    {iterations}");
        println!("  Total time:    {elapsed:?}");
        println!("  Per iteration: {per_iter:?}");
        println!("  Avg runs/frame: {avg_runs}");
        println!("  Avg cells/run: {:.1}", (120.0 * 40.0) / avg_runs as f64);
    }

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_cell_runs_large_terminal() {
        let content = generate_ansi_log(10_000);
        let term = create_term_with_content(240, 80, &content);

        let _ = TerminalElement::build_cell_runs(&term);

        let iterations = 200u32;
        let start = Instant::now();
        let mut total_runs = 0;
        for _ in 0..iterations {
            let runs = TerminalElement::build_cell_runs(&term);
            total_runs += runs.len();
        }
        let elapsed = start.elapsed();
        let per_iter = elapsed / iterations;
        let avg_runs = total_runs / iterations as usize;

        println!("=== Cell Run Construction: Large Terminal (240x80) ===");
        println!("  Terminal size: 240x80");
        println!("  Iterations:    {iterations}");
        println!("  Total time:    {elapsed:?}");
        println!("  Per iteration: {per_iter:?}");
        println!("  Avg runs/frame: {avg_runs}");
        println!("  Avg cells/run: {:.1}", (240.0 * 80.0) / avg_runs as f64);
    }

    // ── Cell Run Cache Benchmarks ─────────────────────────────────────

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_cell_run_cache_hit_rate() {
        let content = generate_ansi_log(10_000);
        let term = create_term_with_content(120, 40, &content);
        let generation = AtomicU64::new(0);

        let mut cache = CellRunCache::new();
        let display_offset = term.grid().display_offset();
        let content_gen = generation.load(Ordering::Relaxed);

        // First access: miss
        let miss_start = Instant::now();
        let runs = TerminalElement::build_cell_runs(&term);
        let build_time = miss_start.elapsed();
        cache.insert(display_offset, content_gen, runs);

        // Subsequent accesses: hit
        let mut hits = 0u64;
        let mut misses = 1u64; // Already had one miss
        let iterations = 1000u32;

        let hit_start = Instant::now();
        for _ in 0..iterations {
            if cache.get(display_offset, content_gen).is_some() {
                hits += 1;
            } else {
                misses += 1;
            }
        }
        let hit_time = hit_start.elapsed();

        println!("=== Cell Run Cache: Same Offset ===");
        println!("  Build time (miss): {build_time:?}");
        println!(
            "  Lookup time (hit): {:?} per lookup",
            hit_time / iterations
        );
        println!("  Hits: {hits}, Misses: {misses}");
        println!(
            "  Hit rate: {:.1}%",
            hits as f64 / (hits + misses) as f64 * 100.0
        );
    }

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_cell_run_cache_scrollback() {
        let content = generate_ansi_log(10_000);
        let mut term = create_term_with_content(120, 40, &content);

        let mut cache = CellRunCache::new();
        let content_gen = 1u64;

        // Simulate scrolling: populate cache with different offsets
        let offsets_to_cache = [0i32, 5, 10, 15, 20, 25, 30, 35];
        for &offset in &offsets_to_cache {
            use alacritty_terminal::grid::Scroll;
            // Reset to top first
            term.scroll_display(Scroll::Top);
            // Then scroll to desired offset
            if offset > 0 {
                term.scroll_display(Scroll::Delta(-offset));
            }
            let display_offset = term.grid().display_offset();
            let runs = TerminalElement::build_cell_runs(&term);
            cache.insert(display_offset, content_gen, runs);
        }

        // Now simulate back-and-forth scrolling pattern: 0, 5, 0, 10, 5, 0, 15, ...
        let scroll_pattern: [usize; 15] = [0, 5, 0, 10, 5, 0, 15, 10, 5, 0, 20, 15, 10, 5, 0];
        let mut hits = 0u64;
        let mut misses = 0u64;

        let iterations = 100u32;
        let start = Instant::now();
        for _ in 0..iterations {
            for &offset in &scroll_pattern {
                if cache.get(offset, content_gen).is_some() {
                    hits += 1;
                } else {
                    misses += 1;
                    // On miss, build and insert
                    use alacritty_terminal::grid::Scroll;
                    term.scroll_display(Scroll::Top);
                    if offset > 0 {
                        #[allow(clippy::cast_possible_wrap)]
                        term.scroll_display(Scroll::Delta(-(offset as i32)));
                    }
                    let display_offset = term.grid().display_offset();
                    let runs = TerminalElement::build_cell_runs(&term);
                    cache.insert(display_offset, content_gen, runs);
                }
            }
        }
        let elapsed = start.elapsed();

        let total = hits + misses;
        println!("=== Cell Run Cache: Scrollback Pattern ===");
        println!("  Cache slots:   8");
        println!("  Scroll pattern: {scroll_pattern:?} (x{iterations})");
        println!("  Total lookups: {total}");
        println!("  Hits: {hits}, Misses: {misses}");
        println!("  Hit rate: {:.1}%", hits as f64 / total as f64 * 100.0);
        println!("  Total time: {elapsed:?}");
    }

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_cell_run_cache_generation_invalidation() {
        let content = generate_ansi_log(10_000);
        let term = create_term_with_content(120, 40, &content);

        let mut cache = CellRunCache::new();
        let display_offset = term.grid().display_offset();

        // Simulate rapid content updates (new PTY output)
        let mut hits = 0u64;
        let mut misses = 0u64;

        let iterations = 1000u64;
        let start = Instant::now();
        for content_gen in 0..iterations {
            if cache.get(display_offset, content_gen).is_some() {
                hits += 1;
            } else {
                misses += 1;
                let runs = TerminalElement::build_cell_runs(&term);
                cache.insert(display_offset, content_gen, runs);
            }
        }
        let elapsed = start.elapsed();

        println!("=== Cell Run Cache: Generation Invalidation ===");
        println!("  Scenario: every frame has new content_generation (tail -f)");
        println!("  Iterations: {iterations}");
        println!("  Hits: {hits}, Misses: {misses}");
        println!(
            "  Hit rate: {:.1}%",
            hits as f64 / (hits + misses) as f64 * 100.0
        );
        println!("  Total time (including rebuilds): {elapsed:?}");
        println!("  Per miss (build + insert): {:?}", elapsed / misses as u32);
    }

    // ── Glyph Cache Benchmarks ────────────────────────────────────────

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_glyph_cache_population() {
        let content = generate_ansi_log(10_000);
        let term = create_term_with_content(120, 40, &content);
        let runs = TerminalElement::build_cell_runs(&term);

        // Count unique (char, bold, italic, wide) combinations
        let mut unique_glyphs = std::collections::HashSet::new();
        let mut total_chars = 0u64;
        for run in &runs {
            for ch in run.text.chars() {
                if ch != ' ' {
                    unique_glyphs.insert((ch, run.bold, run.italic, run.wide));
                    total_chars += 1;
                }
            }
        }

        // Simulate glyph cache behavior (without actual shaping)
        let cache = GlyphCache::new();
        let mut cache_hits = 0u64;
        let mut cache_misses = 0u64;

        let start = Instant::now();
        for run in &runs {
            let color = run.fg;
            for ch in run.text.chars() {
                if ch == ' ' {
                    continue;
                }
                if cache
                    .get(ch, run.bold, run.italic, run.wide, color)
                    .is_some()
                {
                    cache_hits += 1;
                } else {
                    cache_misses += 1;
                    // In real code, shape_line() would be called here.
                    // We just track the miss without shaping since we cannot
                    // access the text system in headless tests.
                }
            }
        }
        let elapsed = start.elapsed();

        // Simulate what hit rate looks like after multiple frames
        // (subsequent frames would have nearly 100% hit rate)
        println!("=== Glyph Cache: Population Analysis ===");
        println!("  Total non-space chars: {total_chars}");
        println!(
            "  Unique (char, style) combos: {} (ignoring color)",
            unique_glyphs.len()
        );
        println!("  First-frame hits:   {cache_hits}");
        println!("  First-frame misses: {cache_misses}");
        println!(
            "  First-frame hit rate: {:.1}%",
            if cache_hits + cache_misses > 0 {
                cache_hits as f64 / (cache_hits + cache_misses) as f64 * 100.0
            } else {
                0.0
            }
        );
        println!("  Lookup time (total): {elapsed:?}");
        println!("  Cache entries (after): {}", cache.len());
        println!();
        println!("  NOTE: After the first frame, all glyphs are cached.");
        println!("  Subsequent frames would show ~100% hit rate with only");
        println!("  HashMap lookups (~5ns each) instead of shape_line() calls.");
    }

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_glyph_cache_lookup_latency() {
        let content = generate_ansi_log(10_000);
        let term = create_term_with_content(120, 40, &content);
        let runs = TerminalElement::build_cell_runs(&term);

        // Measure HashMap lookup latency on an empty cache.
        // This isolates the cost of key hashing + comparison. In production,
        // hits add a pointer dereference to retrieve the ShapedLine, but the
        // HashMap probe cost (measured here) dominates since ShapedLine is
        // accessed by reference.
        let cache = GlyphCache::new();
        let iterations = 1000u32;
        let start = Instant::now();
        let mut lookups = 0u64;
        for _ in 0..iterations {
            for run in &runs {
                let color = run.fg;
                for ch in run.text.chars() {
                    if ch != ' ' {
                        let _ = cache.get(ch, run.bold, run.italic, run.wide, color);
                        lookups += 1;
                    }
                }
            }
        }
        let elapsed = start.elapsed();
        let per_lookup = elapsed / lookups as u32;

        println!("=== Glyph Cache: Lookup Latency (empty map) ===");
        println!("  Iterations: {iterations}");
        println!("  Total lookups: {lookups}");
        println!("  Total time:    {elapsed:?}");
        println!("  Per lookup:    {per_lookup:?}");
        println!("  Lookups/frame: {}", lookups / u64::from(iterations));
        println!();
        println!("  NOTE: Measures HashMap probe cost (hash + compare).");
        println!("  Hit-path adds only a pointer deref to retrieve ShapedLine.");
    }

    // ── End-to-End Pipeline Benchmarks ────────────────────────────────

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_pipeline_tail_f_500lps() {
        // Simulate `tail -f` at 500 lines/second:
        // Each "tick" adds 500/60 ~= 8 lines, and we rebuild cell runs.
        let lines_per_tick = 8;
        let ticks = 300u32; // 5 seconds at 60fps

        let initial_content = generate_ansi_log(1000);
        let mut term = create_term_with_content(120, 40, &initial_content);
        let mut processor: Processor<StdSyncHandler> = Processor::new();

        let mut cache = CellRunCache::new();
        let mut content_gen = 0u64;
        let mut total_build_time = std::time::Duration::ZERO;
        let mut cache_hits = 0u64;
        let mut cache_misses = 0u64;

        let start = Instant::now();
        for tick in 0..ticks {
            // Feed new lines
            let new_content = generate_ansi_log(lines_per_tick);
            processor.advance(&mut term, &new_content);
            content_gen += 1;

            let display_offset = term.grid().display_offset();

            // Try cache first
            if cache.get(display_offset, content_gen).is_some() {
                cache_hits += 1;
            } else {
                cache_misses += 1;
                let build_start = Instant::now();
                let runs = TerminalElement::build_cell_runs(&term);
                total_build_time += build_start.elapsed();
                cache.insert(display_offset, content_gen, runs);
            }

            // Occasionally scroll back and forth
            if tick % 60 == 30 {
                use alacritty_terminal::grid::Scroll;
                term.scroll_display(Scroll::Delta(-10));
                content_gen += 1;
                let display_offset = term.grid().display_offset();
                if cache.get(display_offset, content_gen).is_none() {
                    let runs = TerminalElement::build_cell_runs(&term);
                    cache.insert(display_offset, content_gen, runs);
                    cache_misses += 1;
                } else {
                    cache_hits += 1;
                }
                // Scroll back to bottom
                term.scroll_display(Scroll::Bottom);
            }
        }
        let elapsed = start.elapsed();

        println!("=== Pipeline: tail -f at 500 lines/s (5s simulation) ===");
        println!("  Ticks (frames): {ticks}");
        println!("  Total time:     {elapsed:?}");
        println!("  Build time:     {total_build_time:?}");
        println!(
            "  Avg build/miss: {:?}",
            if cache_misses > 0 {
                total_build_time / cache_misses as u32
            } else {
                std::time::Duration::ZERO
            }
        );
        println!("  Cache hits:     {cache_hits}");
        println!("  Cache misses:   {cache_misses}");
        println!(
            "  Hit rate:       {:.1}%",
            if cache_hits + cache_misses > 0 {
                cache_hits as f64 / (cache_hits + cache_misses) as f64 * 100.0
            } else {
                0.0
            }
        );
        println!("  Per frame avg:  {:?}", elapsed / ticks);
    }

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_pipeline_full_screen_tui() {
        // Simulate full-screen TUI app (htop-like) updating at 30fps
        let frame_count = 150u32; // 5 seconds at 30fps
        let cols = 120;
        let rows = 40;

        let mut term = create_term_with_content(cols, rows, b"");
        let mut processor: Processor<StdSyncHandler> = Processor::new();
        let mut cache = CellRunCache::new();
        let mut content_gen = 0u64;
        let mut total_build_time = std::time::Duration::ZERO;
        let mut cache_misses = 0u64;

        let start = Instant::now();
        for _frame in 0..frame_count {
            // Each frame is a full screen redraw
            let frame_content = generate_tui_frames(1, cols, rows);
            processor.advance(&mut term, &frame_content);
            content_gen += 1;

            let display_offset = term.grid().display_offset();
            if cache.get(display_offset, content_gen).is_none() {
                cache_misses += 1;
                let build_start = Instant::now();
                let runs = TerminalElement::build_cell_runs(&term);
                total_build_time += build_start.elapsed();
                cache.insert(display_offset, content_gen, runs);
            }
        }
        let elapsed = start.elapsed();

        println!("=== Pipeline: Full-Screen TUI (5s at 30fps) ===");
        println!("  Frames: {frame_count}");
        println!("  Total time:     {elapsed:?}");
        println!("  Build time:     {total_build_time:?}");
        println!("  Avg build/frame: {:?}", total_build_time / frame_count);
        println!("  Cache misses: {cache_misses} (every frame is new content)");
        println!("  Per frame avg:  {:?}", elapsed / frame_count);
    }

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_pipeline_large_paste_50k() {
        // Simulate pasting 50K lines into terminal
        let content = generate_large_paste(50_000);
        let content_size = content.len();

        // Feed in chunks (simulating how the PTY would deliver data)
        let chunk_size = 4096;
        let mut term = create_term_with_content(120, 40, b"");
        let mut processor: Processor<StdSyncHandler> = Processor::new();
        let mut cache = CellRunCache::new();
        let mut content_gen = 0u64;
        let mut builds = 0u32;
        let mut total_build_time = std::time::Duration::ZERO;

        let start = Instant::now();
        for chunk in content.chunks(chunk_size) {
            processor.advance(&mut term, chunk);
            content_gen += 1;

            // Build cell runs every 16 chunks (~60fps coalescing at 4KB chunks)
            if content_gen.is_multiple_of(16) {
                let display_offset = term.grid().display_offset();
                let build_start = Instant::now();
                let runs = TerminalElement::build_cell_runs(&term);
                total_build_time += build_start.elapsed();
                cache.insert(display_offset, content_gen, runs);
                builds += 1;
            }
        }
        let elapsed = start.elapsed();
        let vte_time = elapsed.saturating_sub(total_build_time);
        let throughput_mb = (content_size as f64 / 1_000_000.0) / elapsed.as_secs_f64();

        println!("=== Pipeline: Large Paste (50K lines) ===");
        println!(
            "  Content size:   {:.1} MB",
            content_size as f64 / 1_048_576.0
        );
        println!("  Chunk size:     {chunk_size} bytes");
        println!("  Total time:     {elapsed:?}");
        println!("  VTE time:       {vte_time:?}");
        println!("  Build time:     {total_build_time:?}");
        println!("  Builds:         {builds}");
        println!("  Throughput:     {throughput_mb:.1} MB/s");
    }

    // ── Memory Usage Analysis ─────────────────────────────────────────

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_memory_analysis() {
        let content = generate_ansi_log(10_000);
        let term = create_term_with_content(120, 40, &content);
        let runs = TerminalElement::build_cell_runs(&term);

        // Analyze cell run memory usage
        let run_count = runs.len();
        let total_text_bytes: usize = runs.iter().map(|r| r.text.capacity()).sum();
        let total_text_len: usize = runs.iter().map(|r| r.text.len()).sum();

        // Estimate CellRun struct size (without String heap)
        let cell_run_stack_size = std::mem::size_of::<CellRun>();
        let total_stack = run_count * cell_run_stack_size;

        // Cache overhead: 8 slots x (Vec overhead + runs)
        let cache_slot_overhead = std::mem::size_of::<CellRunCacheEntry>();

        // GlyphCache: estimate based on typical entry count
        let glyph_cache_entry_size = std::mem::size_of::<GlyphCacheKey>();
        // ShapedLine size is opaque, estimate ~200 bytes per entry
        let estimated_glyph_entries = 500;
        let estimated_glyph_cache_bytes = estimated_glyph_entries * (glyph_cache_entry_size + 200);

        println!("=== Memory Analysis ===");
        println!("  Cell runs per frame: {run_count}");
        println!("  CellRun struct size: {cell_run_stack_size} bytes");
        println!(
            "  Total stack (runs):  {total_stack} bytes ({:.1} KB)",
            total_stack as f64 / 1024.0
        );
        println!(
            "  Total text heap:     {total_text_bytes} bytes ({:.1} KB)",
            total_text_bytes as f64 / 1024.0
        );
        println!(
            "  Text utilization:    {:.1}% (len/capacity)",
            total_text_len as f64 / total_text_bytes as f64 * 100.0
        );
        println!(
            "  Total per frame:     {:.1} KB",
            (total_stack + total_text_bytes) as f64 / 1024.0
        );
        println!();
        println!("  CellRunCache (8 slots):");
        println!("    Slot overhead: {cache_slot_overhead} bytes each");
        println!(
            "    Total with 8 frames: ~{:.1} KB",
            8.0 * (total_stack + total_text_bytes) as f64 / 1024.0
        );
        println!();
        println!("  GlyphCache:");
        println!("    Key size: {glyph_cache_entry_size} bytes");
        println!("    Estimated entries: ~{estimated_glyph_entries}");
        println!(
            "    Estimated total: ~{:.1} KB",
            estimated_glyph_cache_bytes as f64 / 1024.0
        );
    }

    // ── Comprehensive Summary ─────────────────────────────────────────

    #[test]
    #[ignore = "benchmark: run manually with --ignored"]
    fn bench_summary() {
        println!("=== Terminal Rendering Pipeline Summary ===\n");

        // VTE processing
        let ansi_content = generate_ansi_log(10_000);
        let ansi_size = ansi_content.len();
        let vte_start = Instant::now();
        let term = create_term_with_content(120, 40, &ansi_content);
        let vte_time = vte_start.elapsed();

        // Cell run construction
        let build_start = Instant::now();
        let runs = TerminalElement::build_cell_runs(&term);
        let build_time = build_start.elapsed();

        // Cache lookup
        let mut cache = CellRunCache::new();
        cache.insert(0, 0, runs.clone());
        let lookup_start = Instant::now();
        for _ in 0..1000 {
            let _ = cache.get(0, 0);
        }
        let lookup_time = lookup_start.elapsed() / 1000;

        // Glyph cache lookup
        let glyph_cache = GlyphCache::new();
        let glyph_start = Instant::now();
        let mut glyph_lookups = 0u64;
        for run in &runs {
            for ch in run.text.chars() {
                if ch != ' ' {
                    let _ = glyph_cache.get(ch, run.bold, run.italic, run.wide, run.fg);
                    glyph_lookups += 1;
                }
            }
        }
        let glyph_total = glyph_start.elapsed();
        let per_glyph = if glyph_lookups > 0 {
            glyph_total / glyph_lookups as u32
        } else {
            std::time::Duration::ZERO
        };

        println!("Stage                     | Time           | Notes");
        println!("--------------------------|----------------|---------------------------");
        println!(
            "VTE processing (10K lines)| {:>14?} | {:.0} KB input",
            vte_time,
            ansi_size as f64 / 1024.0
        );
        println!(
            "Cell run construction      | {:>14?} | {} runs, 120x40 grid",
            build_time,
            runs.len()
        );
        println!("Cell run cache lookup      | {lookup_time:>14?} | per lookup (hit)");
        println!(
            "Glyph cache lookup         | {per_glyph:>14?} | per char ({glyph_lookups} total)"
        );
        println!();
        println!("Frame budget at 60fps: 16.6ms");
        println!(
            "Data pipeline (build_cell_runs): {:?} = {:.1}% of frame budget",
            build_time,
            build_time.as_secs_f64() / 0.0166 * 100.0
        );
    }
}
