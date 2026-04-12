# RFC-004: Batched Async Persistence for GUI State

## Status: Draft

## Problem Statement

`crates/zremote-gui/src/persistence.rs` owns the GUI's on-disk state file (`~/.config/zremote/gui-state.json`). Its current shape works well for the tiny state it handles today (window bounds, `server_url`, `active_session_id`, 10-entry `recent_sessions`):

1. `update(|s| ...)` mutates state synchronously and bumps `data_version`.
2. `save_if_changed()` does a blocking atomic-with-backup write on the calling thread if `data_version != last_saved_version`.
3. Call sites today: `lib.rs` init, `lib.rs` quit, `main_view.rs` session switch, `record_session_access` inside persistence itself.

The data volume is small enough that synchronous `fsync` calls are invisible. That changes with the upcoming features:

- **#23 Multi-theme** — persists selected theme on every switch.
- **#26 Command palette (recent actions + context rank)** — persists on *every* palette pick. Under normal usage that's a burst of ~5–20 writes in a second after a rapid session.
- **#27 Extended agent detection** — persists active prompt excerpts as they arrive.
- **#29 AI chat** — persists streaming assistant tokens, potentially hundreds of mutations over a few seconds.
- **#30 Live logs** — persists filter/level preferences less frequently but alongside a busy render loop.

If we keep the synchronous save path, any of these features stalls the GPUI render thread on `fsync` during a burst. On spinning disks, encrypted filesystems, or network home directories, the stall is visible (10–100 ms per write). Worst case: 100 rapid picks in the command palette cause 100 `fsync` calls on the render thread, dropping frames.

The issue (#31) specifies a concrete fix: mutations stay synchronous and non-blocking; a background worker owns all file I/O; bursts are debounced and coalesced; shutdown flushes with a bounded timeout.

## Goals

1. **Non-blocking `update`.** `Persistence::update(|s| ...)` returns in microseconds under all load. No `fsync`, no file I/O on the caller's thread (GPUI main thread in practice).
2. **Single background writer.** Exactly one dedicated thread performs disk I/O. No fan-out, no multi-writer races.
3. **Automatic coalescing.** 100 rapid updates in a 250 ms window produce ≤ 2 actual writes (one for the window, optionally one follow-up).
4. **Bounded shutdown flush.** `flush_blocking(Duration)` blocks on the calling thread until the in-flight + pending writes complete, or the timeout elapses. Drop-time cleanup always flushes with a 2 s deadline.
5. **Durability preserved.** Atomic write, rolling backup (`.json.bak`), "don't save default state", version-bump skip — all retained from the current implementation.
6. **Testability.** Writer is injectable so tests can count writes and assert coalescing without touching real disks.
7. **Backwards-compatible API.** Existing call sites continue to work. `record_session_access`, `update`, `state()`, and `save_if_changed` stay. `save_if_changed` becomes a lightweight "mark dirty + return immediately" that preserves its current Boolean semantics (`true` if a write was queued, `false` if nothing had changed).

## Non-Goals

- Changing the on-disk format, schema version, or backup scheme.
- Cross-process locking (GUI is the sole writer).
- Schema migration logic (only one format version today).
- Splitting the state file into per-feature shards — that is a separate refactor when chat history (#29) actually lands and only if profiling demands it.
- Replacing `Mutex<Persistence>` in `AppState` with an entity-shaped alternative. The batched path is orthogonal to that discussion and can ship first.

## Current State (verified)

`crates/zremote-gui/src/persistence.rs` (179 lines):

```rust
pub struct Persistence {
    path: PathBuf,
    state: GuiState,
    data_version: u64,
    last_saved_version: u64,
}

impl Persistence {
    pub fn load() -> Self { /* read file or default */ }
    pub fn state(&self) -> &GuiState { &self.state }
    pub fn update(&mut self, f: impl FnOnce(&mut GuiState)) {
        f(&mut self.state);
        self.data_version += 1;
    }
    pub fn record_session_access(&mut self, session_id: &str) {
        /* move session to head, cap 10, internal save_if_changed */
    }
    pub fn save_if_changed(&mut self) -> io::Result<bool> { /* blocking fsync */ }
    fn atomic_write(&self) -> io::Result<()> { /* backup + tmp + rename */ }
}
```

Call sites:

- `crates/zremote-gui/src/lib.rs:94` — `persistence.update(|s| s.server_url = ...);` (no explicit save; relies on later save_if_changed in quit path).
- `crates/zremote-gui/src/lib.rs:138-142` — on window close:
  ```rust
  if let Ok(mut p) = app_state_for_quit.persistence.lock() {
      p.update(|s| { /* window bounds */ });
      if let Err(e) = p.save_if_changed() { /* log */ }
  }
  ```
- `crates/zremote-gui/src/views/main_view.rs:174-177` — on session switch:
  ```rust
  if let Ok(mut p) = self.app_state.persistence.lock() {
      p.update(|s| s.active_session_id = Some(session_id_owned.clone()));
      let _ = p.save_if_changed();
  }
  ```
- `crates/zremote-gui/src/views/main_view.rs:1359` — `p.record_session_access(session_id);`

`AppState` holds `persistence: Mutex<Persistence>` (`crates/zremote-gui/src/app_state.rs:22`), so batched saves must be thread-safe and the `Persistence` struct must live across the app's lifetime.

## Architecture

```
┌─────────────────────────┐         ┌────────────────────────────┐
│  Caller (GPUI thread)   │         │  Background saver thread   │
│  ─────────────────────  │         │  ───────────────────────   │
│  Persistence::update()  │         │                            │
│    mutate self.state    │         │     phase 1: wait on cv    │
│    bump data_version    │         │       while pending.none() │
│    clone snapshot       │ signal  │       && !shutdown         │
│    store in pending ────┼────────►│                            │
│    cv.notify_all()      │         │     phase 2: debounce      │
│  returns < 1 μs         │         │       cv.wait_timeout(250) │
│                         │         │                            │
│                         │         │     phase 3: take pending  │
│  flush_blocking(dur):   │         │       set in_flight = v    │
│    wait on cv until     │         │                            │
│    last_saved >= target │◄────────┤     phase 4: write         │
│    or deadline          │ done    │       atomic_write_with_   │
│                         │         │         backup()           │
└─────────────────────────┘         │                            │
                                    │     phase 5: publish       │
                                    │       last_saved = v       │
                                    │       in_flight = 0        │
                                    │       cv.notify_all()      │
                                    └────────────────────────────┘
```

### Data model

```rust
// Public types: unchanged
pub struct GuiState { /* existing fields */ }
impl GuiState { fn is_default(&self) -> bool { /* unchanged */ } }

// Persistence: caller-side handle
pub struct Persistence {
    state: GuiState,
    data_version: u64,
    shared: Arc<SharedSaver>,
    worker: Option<std::thread::JoinHandle<()>>,
}

// Worker-facing shared state
struct SharedSaver {
    mu: std::sync::Mutex<SaverInner>,
    cv: std::sync::Condvar,
    debounce: std::time::Duration,
    writer: Box<dyn PersistenceWriter>,
    path: PathBuf,
    /// Observability / tests.
    writes_performed: std::sync::atomic::AtomicU64,
}

struct SaverInner {
    /// Newest snapshot queued for writing. Overwritten on every update —
    /// only the latest survives, which is the coalescing invariant.
    pending: Option<GuiState>,
    pending_version: u64,
    /// Version currently being written by the worker. 0 = no write in flight.
    in_flight_version: u64,
    last_saved_version: u64,
    shutdown: bool,
}

/// Pluggable writer so tests can observe / inject failures without real I/O.
pub trait PersistenceWriter: Send + Sync {
    fn write(&self, path: &Path, state: &GuiState) -> std::io::Result<()>;
}

/// Production writer: atomic + rolling backup, identical bytes to current impl.
struct FileWriter;
impl PersistenceWriter for FileWriter { /* parent mkdir, backup, tmp+fsync+rename */ }
```

### Worker loop (pseudocode)

```rust
fn run(shared: Arc<SharedSaver>) {
    loop {
        // Phase 1: wait for pending work or shutdown.
        {
            let mut inner = shared.mu.lock().unwrap();
            while inner.pending.is_none() && !inner.shutdown {
                inner = shared.cv.wait(inner).unwrap();
            }
            if inner.shutdown && inner.pending.is_none() {
                break;
            }
        }

        // Phase 2: debounce — release the lock, let additional updates
        // coalesce into the same `pending` slot. Interruptible by shutdown.
        {
            let inner = shared.mu.lock().unwrap();
            let (_guard, _) = shared
                .cv
                .wait_timeout_while(inner, shared.debounce, |st| !st.shutdown)
                .unwrap();
        }

        // Phase 3: take the latest snapshot.
        let taken = {
            let mut inner = shared.mu.lock().unwrap();
            if let Some(snapshot) = inner.pending.take() {
                inner.in_flight_version = inner.pending_version;
                Some((snapshot, inner.in_flight_version))
            } else if inner.shutdown {
                break;
            } else {
                None
            }
        };
        let Some((snapshot, version)) = taken else { continue; };

        // Phase 4: write OUTSIDE the lock. Skip default-state to match current behaviour.
        let result = if snapshot.is_default() {
            Ok(())
        } else {
            shared.writer.write(&shared.path, &snapshot)
        };

        // Phase 5: publish result, notify flushers.
        {
            let mut inner = shared.mu.lock().unwrap();
            match &result {
                Ok(()) => {
                    inner.last_saved_version = version;
                    shared.writes_performed.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => tracing::warn!(error = %e, path = %shared.path.display(),
                                         "persistence worker write failed"),
            }
            inner.in_flight_version = 0;
            shared.cv.notify_all();
        }
    }
    tracing::debug!("persistence worker exiting");
}
```

### Caller-side API

```rust
impl Persistence {
    /// Load state from disk and spawn the background writer.
    /// Default debounce: 250 ms. Default writer: FileWriter.
    pub fn load() -> Self { /* existing load logic + spawn worker */ }

    /// Test / advanced constructor with injected writer and debounce.
    pub fn with_writer(
        path: PathBuf,
        writer: Box<dyn PersistenceWriter>,
        debounce: Duration,
    ) -> Self { /* ... */ }

    pub fn state(&self) -> &GuiState { &self.state }

    /// Mutate state, bump version, queue a save. Non-blocking: returns after
    /// touching only in-memory fields and the saver mutex.
    pub fn update(&mut self, f: impl FnOnce(&mut GuiState)) {
        f(&mut self.state);
        self.data_version += 1;
        self.queue_save();
    }

    /// Record a session access. Identical semantics to today; internally
    /// calls `update` (so the save is queued, not synchronous).
    pub fn record_session_access(&mut self, session_id: &str) { /* same as today */ }

    /// Back-compat shim. With batched persistence, there is nothing to do
    /// at the call site beyond ensuring the queued save reaches disk —
    /// which the worker already does. Returns `true` if the most-recent
    /// data_version is not yet on disk, `false` if everything is flushed.
    /// NEVER blocks.
    pub fn save_if_changed(&mut self) -> std::io::Result<bool> { /* ... */ }

    /// Block the current thread until every mutation up to `self.data_version`
    /// is on disk, or `timeout` elapses. Returns Err on timeout.
    pub fn flush_blocking(&mut self, timeout: Duration) -> Result<(), FlushTimeout> {
        let target = self.data_version;
        let deadline = Instant::now() + timeout;
        let mut inner = self.shared.mu.lock().unwrap();
        while inner.last_saved_version < target && !inner.shutdown {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(FlushTimeout);
            };
            let (new_inner, result) = self.shared.cv.wait_timeout(inner, remaining).unwrap();
            inner = new_inner;
            if result.timed_out() && inner.last_saved_version < target {
                return Err(FlushTimeout);
            }
        }
        Ok(())
    }

    fn queue_save(&mut self) {
        let snapshot = self.state.clone();
        let version = self.data_version;
        let mut inner = self.shared.mu.lock().unwrap();
        inner.pending = Some(snapshot);
        inner.pending_version = version;
        self.shared.cv.notify_all();
    }
}

impl Drop for Persistence {
    fn drop(&mut self) {
        // Best-effort flush with a 2 s deadline.
        let _ = self.flush_blocking(Duration::from_secs(2));
        // Signal shutdown and join the worker.
        {
            let mut inner = self.shared.mu.lock().unwrap();
            inner.shutdown = true;
        }
        self.shared.cv.notify_all();
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}
```

### Invariants

- **Single writer.** Only the worker thread calls `PersistenceWriter::write`. The caller never touches disk after `load()`.
- **Monotonic versions.** `last_saved_version <= pending_version` always. `last_saved_version` never decreases.
- **Coalescing.** If `update` is called N times while a save is pending or in flight, the worker performs at most 2 writes: one for whatever was in `pending` when the current write started, plus at most one more for everything after.
- **Shutdown-safe.** After `Drop` returns, the worker thread has joined and no background I/O is possible.
- **No deadlock on lock ordering.** Only one Mutex (`shared.mu`). No nested locks. Worker never holds `mu` during disk I/O.
- **Default-state guard preserved.** Worker skips writing if the snapshot is `is_default()`. Matches today's behaviour precisely.

### Error handling

- **Worker write failure** → logged at `warn` level, `last_saved_version` stays unchanged. Next update triggers a retry on the same or newer version. No panic, no `unwrap` on I/O.
- **Flush timeout** → `FlushTimeout` error returned to caller. Caller logs and proceeds (shutdown path already tolerates write failures).
- **Mutex poisoning** → matches current code: `.unwrap()` on the lock. Justification: the worker is the only critical section, and a poisoned persistence mutex means the process is already dead. Documented with a comment at each `.unwrap()` site.
- **Worker panic** → the `JoinHandle::join()` in `Drop` returns `Err`. Logged, not propagated. Process continues to exit.

## Phases

### Phase 1 — Rewrite `persistence.rs` with batched worker

**Create / modify:**

- `crates/zremote-gui/src/persistence.rs` — full rewrite preserving `GuiState`, `RecentSession`, and public method signatures.

**Behaviour:**

- `Persistence::load()` reads the file (identical parse logic), spawns the worker thread, returns.
- `update`, `record_session_access`, `save_if_changed`, `state()`, `flush_blocking` per the Architecture section above.
- `atomic_write_with_backup` moved out of `impl Persistence` into a free function consumed by `FileWriter`, identical byte layout.
- `Drop` flushes then shuts down the worker.

**Constraint:** No changes to `GuiState` fields, no JSON-schema change. The format version stays `1`. An existing `gui-state.json` loads and saves identically.

### Phase 2 — Adapt call sites

**Modify:**

- `crates/zremote-gui/src/lib.rs:138-146` — the quit path currently calls `save_if_changed()` and logs errors. With batched persistence, the right primitive is `flush_blocking(Duration::from_secs(2))` so the window-bounds update actually reaches disk before the process exits. Replace the call.
- `crates/zremote-gui/src/views/main_view.rs:174-177` — drop the `let _ = p.save_if_changed();` line. `update` now queues the save. The line becomes pure noise.
- `crates/zremote-gui/src/lib.rs:94` — already only calls `update`, no change.

**Constraint:** No API changes visible to anyone outside `persistence.rs`. The `save_if_changed` method stays for back-compat; we just stop calling it in the trivial cases.

### Phase 3 — Tests

**Create:** `#[cfg(test)] mod tests` at the bottom of `crates/zremote-gui/src/persistence.rs`.

Test matrix (mandatory):

1. **`test_update_is_nonblocking`** — with a writer that sleeps 500 ms per write, issue 20 updates in a loop. The whole loop must complete in well under 100 ms.
2. **`test_coalescing_100_updates`** — with a counting writer + short debounce, issue 100 rapid updates, flush, assert `writes_performed <= 2`.
3. **`test_flush_blocking_returns_after_save`** — single update, flush_blocking(1s), verify writer was called once and `last_saved_version == data_version`.
4. **`test_flush_blocking_timeout`** — writer that blocks until signaled, flush_blocking(50ms) → `Err(FlushTimeout)`.
5. **`test_drop_flushes_pending_writes`** — update, then drop Persistence with the counting writer; assert writer saw the write.
6. **`test_write_failure_does_not_advance_last_saved`** — writer that returns `Err`, update, flush_blocking(short) returns `Err(FlushTimeout)` because last_saved_version never advances.
7. **`test_default_state_is_not_written`** — fresh `Persistence` in a temp dir, no updates, flush, writer never called.
8. **`test_record_session_access_queues_save`** — call record_session_access, flush, writer sees a snapshot whose `recent_sessions[0].session_id` matches.
9. **`test_worker_joins_on_drop`** — writer with an Arc counter; after Persistence drops, the counter is correct and the worker thread has terminated.
10. **`test_burst_then_idle_then_burst`** — update burst, flush, second burst, flush; `writes_performed` between 2 and 4.

**Test helpers:**

```rust
struct CountingWriter {
    calls: Arc<AtomicU64>,
    last_written: Arc<Mutex<Option<GuiState>>>,
}
impl PersistenceWriter for CountingWriter { /* increment + store snapshot */ }

struct BlockingWriter { /* blocks until a channel fires */ }
struct FailingWriter { /* always returns Err(ErrorKind::Other) */ }
```

**Debounce in tests:** use 10–50 ms, not the production 250 ms, to keep wall-clock time short.

### Phase 4 — Wire-up verification

**Manual + automated:**

- `cargo build --workspace` — succeeds.
- `cargo clippy --workspace -- -D warnings` — no new lints.
- `cargo test -p zremote-gui` — all tests, including the new ones, pass.
- `cargo test --workspace` — nothing else regresses.
- Manual: run the GUI (`cargo run -p zremote -- gui --local`), change the window size, close the window, reopen. Window size restored.
- Manual: navigate between sessions 10 times rapidly. No visible stutter. `~/.config/zremote/gui-state.json` reflects the latest selection after close.

## Risks

| Risk | Mitigation |
|---|---|
| Worker panic leaves the GUI writing nothing forever | Panic is logged, `Drop` still joins. Any future update would push `pending` but no writer would drain it. Acceptable: a panicked worker is a bug we must fix, and the GUI continues running. |
| `flush_blocking` deadlock if the mutex is poisoned | `.unwrap()` on the lock panics, which is equivalent to today's behaviour. Documented. |
| Test flakiness from real wall-clock debounce | Tests use 10–50 ms debounce and rely on ordering through cv notifications, not time alone. Flush uses `last_saved_version` monotonicity for correctness, not sleeps. |
| Cloning GuiState on every update becomes expensive later when chat history lands | Explicit non-goal here; split the state file when that happens. Current GuiState is < 1 KB, clone is free. |
| Existing call sites that read `state()` right after `update()` see the new state (correct) but not its on-disk counterpart (expected) | Documented: `state()` is in-memory truth. Disk truth lags by ≤ debounce + one `fsync`. No caller depends on disk truth. |

## Acceptance criteria (maps to issue #31)

- [ ] `update(...)` returns without touching disk I/O.
- [ ] Exactly one background thread performs file writes.
- [ ] Debounce configurable, default 250 ms.
- [ ] Test `test_coalescing_100_updates` asserts ≤ 2 writes.
- [ ] `flush_blocking(timeout)` bounded and returns Err on timeout.
- [ ] `Drop::drop` flushes with a 2 s deadline.
- [ ] `atomic_write`, rolling `.bak`, skip-default behaviour preserved — verified by a byte-for-byte comparison test against a hand-written reference file.
- [ ] `record_session_access` unchanged externally.
- [ ] All call sites in `lib.rs` / `main_view.rs` updated.
- [ ] Module-level doc comment explains the model, invariants, and testing helpers.
- [ ] Ready for consumption by #23, #26, #29 — no further changes needed.

## Out of scope

- Changing the persistence file name, location, or schema.
- Migrating away from `Mutex<Persistence>` in `AppState`.
- Compressing the on-disk format.
- Cross-machine state sync.

## Rollout

Land in a single PR that bundles phases 1–3. Phase 4 is the CI matrix plus a manual smoke test. No feature flag. No migration step — the on-disk format is unchanged.

## References

- Issue: #31
- Current file: `crates/zremote-gui/src/persistence.rs`
- Call sites: `lib.rs:94`, `lib.rs:138-146`, `views/main_view.rs:174-177`, `views/main_view.rs:1359`
- AppState glue: `app_state.rs:22`
- Related future consumers: #23 (theme), #26 (recent actions), #27 (agent prompt), #29 (chat history), #30 (log filters)
