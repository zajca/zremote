# RFC: ZRemote Mobile App -- Post-MVP Improvements

## Status: Planned

## Context

The ZRemote Android MVP (Phase 0-4) is complete with:
- `zremote-ffi` Rust crate (UniFFI bindings, 60+ API methods, callback interfaces)
- Android build pipeline (cargo-ndk, CI workflow, size-optimized profile)
- Jetpack Compose app (hosts, sessions, loops, tasks, terminal viewer, settings)
- Push notifications via foreground service (client-driven, Option A)

This RFC documents known gaps, quality improvements, and feature enhancements discovered during code review. Items are prioritized by impact on user experience and app stability.

---

## P0: Must Fix (blocking quality issues)

### 1. Empty states on all list screens

**Problem:** Only `HostListScreen` shows an empty state message. `LoopListScreen`, `TaskListScreen`, and `SessionListScreen` render a blank screen when there's no data. Users can't tell if data is loading, missing, or if there's an error.

**Files to modify:**
- `android/app/src/main/java/com/zremote/ui/screens/loops/LoopListScreen.kt`
- `android/app/src/main/java/com/zremote/ui/screens/tasks/TaskListScreen.kt`
- `android/app/src/main/java/com/zremote/ui/screens/sessions/SessionListScreen.kt`

**Solution:** Extract reusable `EmptyState` and `ErrorState` composables. Each screen should show:
- Loading: `CircularProgressIndicator` centered
- Empty: icon + message + action hint (e.g., "No active loops")
- Error: error message + retry button

**Pattern (from HostListScreen):**
```kotlin
@Composable
fun EmptyState(icon: ImageVector, message: String, hint: String? = null) {
    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Icon(icon, contentDescription = null, modifier = Modifier.size(48.dp))
            Text(message, style = MaterialTheme.typography.bodyLarge)
            hint?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        }
    }
}
```

---

### 2. Error handling in all ViewModels

**Problem:** `SessionListViewModel`, `LoopListViewModel`, `LoopDetailViewModel`, and `TaskListViewModel` silently swallow exceptions:
```kotlin
catch (_: Exception) {
    _sessions.value = emptyList()  // User sees nothing, no idea what went wrong
}
```

Only `HostListViewModel` properly exposes errors via `_error` StateFlow.

**Files to modify:**
- `android/app/src/main/java/com/zremote/ui/screens/sessions/SessionListViewModel.kt`
- `android/app/src/main/java/com/zremote/ui/screens/loops/LoopListViewModel.kt`
- `android/app/src/main/java/com/zremote/ui/screens/loops/LoopDetailViewModel.kt`
- `android/app/src/main/java/com/zremote/ui/screens/tasks/TaskListViewModel.kt`

**Solution:** Add `_error: MutableStateFlow<String?>` to every ViewModel. Set it in catch blocks. Display in screens via `ErrorState` composable with retry action.

---

### 3. Pull-to-refresh on all list screens

**Problem:** Only `HostListScreen` has `PullToRefreshBox`. Other list screens have no way to manually refresh data.

**Files to modify:**
- `android/app/src/main/java/com/zremote/ui/screens/sessions/SessionListScreen.kt`
- `android/app/src/main/java/com/zremote/ui/screens/loops/LoopListScreen.kt`
- `android/app/src/main/java/com/zremote/ui/screens/tasks/TaskListScreen.kt`

**Solution:** Wrap `LazyColumn` in `PullToRefreshBox` in all list screens. Each ViewModel already has a `refresh()` or `load*()` method.

---

### 4. Loading state indicators

**Problem:** ViewModels track `_isLoading` but screens don't display it. No visual feedback during API calls.

**Files to modify:** Same as #3 (screens).

**Solution:** Show `CircularProgressIndicator` when `isLoading == true && items.isEmpty()`. When items exist, show pull-to-refresh indicator.

---

## P1: Should Fix (significant UX improvements)

### 5. Projects screen

**Problem:** `android/app/src/main/java/com/zremote/ui/screens/projects/` directory exists but is empty. Projects are a P1 feature in the original RFC.

**Files to create:**
- `ProjectListScreen.kt`
- `ProjectListViewModel.kt`

**Features:**
- List projects per host with git branch, dirty status, ahead/behind
- Show `has_claude_config` / `has_zremote_config` badges
- Tap to view project details (settings, worktrees, actions)
- Add Projects tab to bottom navigation (or as sub-screen under Hosts)

**API methods available in SDK:**
- `listProjects(hostId)` -> `Vec<FfiProject>`
- `getProject(projectId)` -> `FfiProject`
- `triggerGitRefresh(projectId)`
- `triggerScan(hostId)`
- `listWorktrees(projectId)` -> `Vec<FfiWorktreeInfo>`

---

### 6. Task detail screen with approve/deny

**Problem:** `TaskListScreen` shows tasks but there's no detail view. The original RFC P0 scope includes approve/deny for tool call permissions, which requires a task detail screen.

**Files to create:**
- `android/app/src/main/java/com/zremote/ui/screens/tasks/TaskDetailScreen.kt`
- `android/app/src/main/java/com/zremote/ui/screens/tasks/TaskDetailViewModel.kt`

**Features:**
- Task metadata: model, prompt, cost, tokens, duration
- Loop association (link to loop detail)
- Summary display when completed
- Approve/deny buttons for pending permission requests

**Dependency:** Server-side permission forwarding API. Currently no endpoint exists to approve/deny tool calls from mobile. Needs:
- `POST /api/loops/{loop_id}/approve` -- approve pending tool call
- `POST /api/loops/{loop_id}/deny` -- deny pending tool call
- `GET /api/loops/{loop_id}/pending-permissions` -- list pending permissions

This is a **server-side prerequisite** -- must be implemented in `zremote-server` and `zremote-agent` before the mobile approve/deny UI works.

---

### 7. Server-driven push notifications (FCM)

**Problem:** Current implementation uses a foreground service (Option A) which:
- Drains battery (keeps WebSocket + CPU active)
- Shows persistent notification bar icon
- Android may kill the service after extended background time
- Doesn't work when app is force-closed

**Current state:**
- `ZRemoteEventService` (foreground service) -- works but battery-intensive
- `NotificationEventListener` -- fires local notifications

**Target architecture (Option B):**
```
Server --> FCM --> Android device --> notification
```

**Server-side changes needed:**
1. New endpoint: `POST /api/notifications/register`
   - Body: `{ device_token: String, platform: "android"|"ios", preferences: {...} }`
2. New table: `notification_registrations`
   - Fields: `id, device_token, platform, preferences_json, created_at, updated_at`
3. New module: FCM dispatch
   - On relevant ServerEvent, check registered devices, send via FCM HTTP v1 API
   - Rate limiting to avoid notification spam

**Android-side changes:**
1. Add Firebase Cloud Messaging dependency
2. `FirebaseMessagingService` to receive FCM tokens and messages
3. Token registration on connect (call new server endpoint)
4. Notification preferences screen (which events to notify about)
5. Remove foreground service (or make it optional for real-time updates)

**Firebase project setup:**
- Create Firebase project at console.firebase.google.com
- Download `google-services.json` to `android/app/`
- Add Firebase BOM to dependencies

**Effort:** ~2 weeks (server + mobile)

---

### 8. Reusable UI component library

**Problem:** Common UI patterns are duplicated across screens (status dots, card layouts, detail rows). No shared component library.

**Solution:** Create `android/app/src/main/java/com/zremote/ui/components/`:

| Component | Used by | Description |
|-----------|---------|-------------|
| `StatusDot` | HostCard, SessionCard, LoopCard | Colored circle for online/offline/status |
| `EmptyState` | All list screens | Icon + message + hint centered |
| `ErrorState` | All list screens | Error message + retry button |
| `LoadingState` | All screens | Centered progress indicator |
| `DetailRow` | LoopDetail, TaskDetail | Label + value pair |
| `RefreshableList` | All list screens | LazyColumn + PullToRefreshBox wrapper |

---

## P2: Nice to Have (polish and advanced features)

### 9. VT100 terminal emulation

**Problem:** Current terminal viewer only parses ANSI SGR (color/bold) escape sequences. No cursor positioning, no scrollback region, no alternate screen buffer.

**Current state:** `AnsiParser.kt` handles:
- SGR codes 0-107 (reset, bold, 8-color, 256-color)
- Text output with styled characters

**Missing:**
- Cursor movement (CUP, CUU, CUD, CUF, CUB)
- Erase in display/line (ED, EL)
- Scroll regions (DECSTBM)
- Alternate screen buffer (DECSET 1049)
- Character set switching (SCS)
- Window title (OSC)

**Options:**
1. **Integrate termux terminal-emulator** -- mature Android terminal library, handles full VT100+
2. **Port alacritty_terminal to Kotlin** -- complex but consistent with desktop
3. **Use a WASM-compiled terminal** -- run xterm.js in a WebView for the terminal area only

**Recommendation:** Option 1 (termux) for production quality. Current ANSI parser is sufficient for read-only log viewing.

---

### 10. Session creation from mobile

**Problem:** Users can't create new terminal sessions from the mobile app. They must create sessions from the desktop client or CLI.

**Files to create:**
- Dialog or bottom sheet in `SessionListScreen`
- Uses `client.createSession(hostId, FfiCreateSessionRequest(...))`

**UI:** Floating action button on session list -> bottom sheet with:
- Shell selection (optional)
- Working directory (optional, with `browseDirectory` API)
- Terminal dimensions (auto-detect from screen size)

---

### 11. Gradle wrapper

**Problem:** No `gradle-wrapper.jar` or `gradle-wrapper.properties` checked in. Developers need Gradle pre-installed.

**Solution:** Run `gradle wrapper --gradle-version 8.11` in the `android/` directory to generate:
- `gradle/wrapper/gradle-wrapper.jar`
- `gradle/wrapper/gradle-wrapper.properties`
- `gradlew` (Unix)
- `gradlew.bat` (Windows)

Check these into git. Add `gradle-wrapper.jar` exception to `.gitignore`.

---

### 12. ProGuard rules

**Problem:** Current ProGuard rules only keep `com.zremote.sdk.**`. Release builds may strip:
- Hilt-generated classes
- Kotlin serialization metadata
- Compose-related reflection targets

**File to modify:** `android/app/proguard-rules.pro`

**Additional rules needed:**
```proguard
# Kotlin serialization
-keepattributes *Annotation*, InnerClasses
-dontnote kotlinx.serialization.**
-keepclassmembers class kotlinx.serialization.json.** { *** Companion; }
-keepclasseswithmembers class kotlinx.serialization.json.** {
    kotlinx.serialization.KSerializer serializer(...);
}
-keep,includedescriptorclasses class com.zremote.**$$serializer { *; }
-keepclassmembers class com.zremote.** {
    *** Companion;
}

# Hilt
-keep class dagger.hilt.** { *; }
-keep class javax.inject.** { *; }
-keep class * extends dagger.hilt.android.internal.managers.ViewComponentManager$FragmentContextWrapper { *; }
```

---

### 13. Notification preferences screen

**Problem:** Users can't control which events trigger notifications. All events notify at hardcoded priorities.

**Solution:** Settings screen section with toggles per notification channel:
- Loop completions (on/off)
- Loop errors (on/off)
- Permission requests (on/off, always recommended on)
- Task completions (on/off)
- Task errors (on/off)
- Host disconnections (on/off)

Store in DataStore preferences, check before sending notification.

---

### 14. Offline mode / caching

**Problem:** App shows nothing when offline. No cached data from previous sessions.

**Solution:** Add Room database for local caching:
- Cache last-known host list, session list, loop list
- Show stale data with "last updated X ago" badge when disconnected
- Sync on reconnect

**Effort:** ~1 week

---

### 15. QR code pairing

**Problem:** Users must manually type the server URL. Error-prone, especially for IP addresses.

**Solution:** Server generates a QR code (via web UI or CLI) containing:
```json
{"url": "http://192.168.1.100:3000", "token": "optional-auth-token"}
```

Mobile app scans QR code via `CameraX` + ML Kit barcode scanner, auto-fills settings.

**Effort:** ~3 days (mobile) + QR generation on server side

---

## Implementation Priority

| Phase | Items | Effort |
|-------|-------|--------|
| **Next sprint** | #1 Empty states, #2 Error handling, #3 Pull-to-refresh, #4 Loading states | 2-3 days |
| **Sprint +1** | #5 Projects screen, #8 Component library, #11 Gradle wrapper, #12 ProGuard | 3-4 days |
| **Sprint +2** | #6 Task detail + approve/deny (requires server API), #10 Session creation | 1-2 weeks |
| **Later** | #7 FCM push, #9 VT100 emulation, #13 Notification prefs, #14 Offline cache, #15 QR pairing | 3-5 weeks |
