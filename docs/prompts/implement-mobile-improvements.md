# Agent Prompt: Implement Mobile App Post-MVP Improvements

## Your Role

You are **team lead** for implementing `docs/rfc/rfc-mobile-app-improvements.md`. You plan, delegate to teammates, review, and merge. Follow the Implementation Workflow in `CLAUDE.md` exactly.

## Context

The ZRemote Android MVP was completed in PR #9 (`worktree-rfc-mobile-update` branch). It includes:
- `crates/zremote-ffi/` -- Rust UniFFI bindings (60+ API methods, EventListener/TerminalListener callbacks)
- `android/` -- Jetpack Compose app (Hilt DI, 6 screens, terminal viewer, foreground service notifications)
- `scripts/build-android.sh` -- cargo-ndk cross-compilation + Kotlin binding generation
- `.github/workflows/android-build.yml` -- CI for Android builds

The RFC at `docs/rfc/rfc-mobile-app-improvements.md` documents 15 improvements discovered during code review. You will implement them in phased sprints.

## Instructions

### Step 0: Setup

1. **Enter worktree** -- always work in an isolated worktree, never modify main directly
2. **Read the RFC** at `docs/rfc/rfc-mobile-app-improvements.md` -- read the entire document
3. **Read CLAUDE.md** -- understand the project's development workflow, coding conventions, and mandatory review process
4. **Explore current Android code** -- read all files under `android/app/src/main/java/com/zremote/` to understand existing patterns, especially `HostListScreen.kt` (it has the correct patterns for empty states, pull-to-refresh, error handling that other screens are missing)
5. **Create team** via `TeamCreate`
6. **Create tasks** via `TaskCreate` with dependencies matching the phased plan below

### Step 1: Sprint 1 -- P0 Quality Fixes (Items #1-4 + #8)

These are all Android/Kotlin changes. No Rust changes needed.

**Phase 1A: Component library (#8 partial -- prerequisite for other fixes)**

Create reusable composables FIRST since items #1-4 all need them:
- `android/app/src/main/java/com/zremote/ui/components/StatusDot.kt` -- colored circle, takes color parameter
- `android/app/src/main/java/com/zremote/ui/components/EmptyState.kt` -- icon + message + optional hint, centered
- `android/app/src/main/java/com/zremote/ui/components/ErrorState.kt` -- error message + retry button
- `android/app/src/main/java/com/zremote/ui/components/LoadingState.kt` -- centered CircularProgressIndicator
- `android/app/src/main/java/com/zremote/ui/components/RefreshableList.kt` -- PullToRefreshBox + LazyColumn wrapper
- `android/app/src/main/java/com/zremote/ui/components/DetailRow.kt` -- label + value pair (extract from LoopDetailScreen)

Reference `HostListScreen.kt` for the existing empty state pattern. Make components generic and reusable.

**Phase 1B: Fix ViewModels (#2 -- error handling)**

Add `_error: MutableStateFlow<String?>` and proper error exposure to:
- `SessionListViewModel.kt` -- currently uses `catch (_: Exception) { _sessions.value = emptyList() }`
- `LoopListViewModel.kt` -- same silent catch pattern
- `LoopDetailViewModel.kt` -- same
- `TaskListViewModel.kt` -- same

Pattern to follow (from `HostListViewModel.kt`):
```kotlin
private val _error = MutableStateFlow<String?>(null)
val error: StateFlow<String?> = _error.asStateFlow()

fun refresh() {
    viewModelScope.launch {
        _isLoading.value = true
        _error.value = null
        try { ... }
        catch (e: Exception) { _error.value = e.message }
        finally { _isLoading.value = false }
    }
}
```

**Phase 1C: Fix Screens (#1 empty states, #3 pull-to-refresh, #4 loading)**

Update all list screens to use the new components:
- `SessionListScreen.kt` -- add PullToRefreshBox, EmptyState, ErrorState, LoadingState
- `LoopListScreen.kt` -- same
- `TaskListScreen.kt` -- same

Also refactor `HostListScreen.kt` to use the shared components instead of inline composables.

**Review after Sprint 1:**
- Spawn `code-reviewer` to verify consistency across all screens
- Verify: every list screen has loading, empty, error, and pull-to-refresh states
- Commit with descriptive message

### Step 2: Sprint 2 -- P1 Features (Items #5, #8 complete, #11, #12)

**Phase 2A: Complete component library (#8)**

Refactor existing screens to use shared components:
- Replace inline `StatusDot` (Surface+CircleShape) in HostCard, SessionCard with `StatusDot` component
- Replace `DetailRow` in LoopDetailScreen with shared component
- Verify no duplicated UI patterns remain

**Phase 2B: Projects screen (#5)**

Create:
- `android/app/src/main/java/com/zremote/ui/screens/projects/ProjectListScreen.kt`
- `android/app/src/main/java/com/zremote/ui/screens/projects/ProjectListViewModel.kt`

Features:
- List projects per host: name, path, git branch, dirty status, ahead/behind counts
- Show badges for `has_claude_config` / `has_zremote_config`
- Pull-to-refresh, empty state, error handling (using shared components)
- Navigation: add as sub-screen under host detail, or add Projects tab to bottom navigation

Use API: `client.listProjects(hostId)`, `client.triggerGitRefresh(projectId)`, `client.triggerScan(hostId)`

Wire into `NavGraph.kt` with a new `ProjectsRoute(hostId: String)`.

**Phase 2C: Build infrastructure (#11, #12)**

- Gradle wrapper: if Gradle is available, run `gradle wrapper --gradle-version 8.11` in `android/`. If not, create the properties file manually pointing to Gradle 8.11 distribution.
- ProGuard rules: update `android/app/proguard-rules.pro` with rules from RFC (Kotlin serialization, Hilt, Compose keeps)

**Review after Sprint 2:**
- `code-reviewer` for architecture and consistency
- Verify Projects screen follows same patterns as other screens
- Commit

### Step 3: Sprint 3 -- P1 Advanced (Items #6, #10)

**Phase 3A: Task detail screen (#6)**

Create:
- `android/app/src/main/java/com/zremote/ui/screens/tasks/TaskDetailScreen.kt`
- `android/app/src/main/java/com/zremote/ui/screens/tasks/TaskDetailViewModel.kt`

Features:
- Display task metadata: model, initial prompt, cost, tokens in/out, started/ended times
- Show summary when completed
- Link to associated loop (navigate to LoopDetailScreen)
- Loading/error states

Wire `TaskListScreen` cards to navigate to detail. Add `TaskDetailRoute(taskId: String)` to NavGraph.

Note: Approve/deny buttons are **blocked by server-side API** (endpoints don't exist yet). Add a placeholder section in the UI saying "Permission approval requires server v0.8+" or similar. Do NOT mock the API.

**Phase 3B: Session creation (#10)**

Add to `SessionListScreen.kt`:
- FloatingActionButton that opens a bottom sheet
- Bottom sheet with: shell selection (optional text field), working directory (optional), terminal dimensions (auto-detect from screen)
- On submit: call `client.createSession(hostId, FfiCreateSessionRequest(...))`, then navigate to `TerminalRoute(session.id)`

**Review after Sprint 3:**
- `code-reviewer` for architecture
- Verify all new screens have loading/empty/error states
- Commit

### Step 4: Sprint 4 -- P2 Polish (Items #13)

**Phase 4A: Notification preferences (#13)**

Add to Settings screen:
- Section "Notifications" with toggles per channel:
  - Loop completions, Loop errors, Permission requests, Task completions, Task errors, Host disconnections
- Store in DataStore preferences
- Update `NotificationEventListener.kt` to check preferences before sending

**Review and commit.**

### Items NOT to implement (deferred)

These items require external dependencies or major architecture changes beyond this RFC scope:
- **#7 FCM push** -- requires Firebase project setup + server-side changes (new Axum endpoints, FCM dispatch module, SQL migration). Create a separate RFC.
- **#9 VT100 emulation** -- requires integrating termux terminal-emulator library or equivalent. Current ANSI parser is sufficient for MVP.
- **#14 Offline cache** -- requires adding Room database dependency and caching layer. Separate feature.
- **#15 QR pairing** -- requires CameraX + ML Kit + server-side QR generation. Separate feature.

If you encounter a situation where one of these is needed to complete an in-scope item, document the dependency and move on.

## Rules (from CLAUDE.md -- mandatory)

1. **Always work in a worktree** -- `EnterWorktree` before any changes
2. **Read before modify** -- always read a file before editing it
3. **No mocks** -- real implementations only. If blocked by missing server API, add placeholder UI text explaining the dependency
4. **No skipping** -- every item assigned to a sprint must be fully implemented
5. **Code review is mandatory** -- spawn `code-reviewer` after each sprint. Fix ALL findings before committing
6. **Tests** -- Kotlin unit tests are not required for Compose UI (no test infrastructure set up yet), but if you add business logic utilities (e.g., date formatting, status mapping), write tests
7. **Commit messages** -- descriptive, reference RFC item numbers
8. **Verify Rust crate** -- after any changes, run `cargo test -p zremote-ffi && cargo clippy -p zremote-ffi` to ensure nothing is broken
9. **PR at the end** -- create a PR with summary of all changes, referencing the RFC

## Expected Output

At the end, the worktree should have:
- Sprint commits (one per sprint, squash if needed)
- All P0 items (#1-4) fully implemented
- All P1 items (#5, #6 partial, #8, #10, #11, #12) implemented
- P2 item #13 implemented
- Updated RFC with completion status for each item
- PR created targeting `main`
