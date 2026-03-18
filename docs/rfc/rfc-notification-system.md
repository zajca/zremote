# RFC: Comprehensive Notification System

## Context & Problem

ZRemote's notification coverage is ~15-20%. Out of 23 ServerEvent types, only 1 (WorktreeError) triggers a user notification. Out of 50+ API endpoints, only ~10 show toasts on success/failure. When Claude needs user input (pending tool approval, waiting_for_input), the user must be on the correct page to notice - there's no global notification.

**Goal**: Build a comprehensive notification system with three tiers:
1. **Action toasts** - persistent, with action buttons (Approve/Reject/Go to Terminal) for events needing user response
2. **Info toasts** - auto-dismiss, for events the user should know about (host disconnect, session suspend, task completion, API errors)
3. **Browser notifications** - Web Notifications API when the tab is not focused

The existing `notifications.enabled` config stub in Settings will be wired to control this system.

## Current State

### Existing Toast System (`web/src/components/layout/Toast.tsx`)
- Simple pub-sub: `showToast(message, type)` where type is `"error" | "info" | "success"`
- Auto-dismiss: 4s success, 8s error/info. No action buttons.
- 24 `showToast()` calls across 9 components

### Places With Notifications (adequate)
| Location | Events |
|----------|--------|
| `SessionPage.tsx` | Close, rename, clipboard copy |
| `SessionItem.tsx` | Close from sidebar |
| `HostPage.tsx` | Session create failure |
| `AgenticLoopPanel.tsx` | Action approve/reject/send failure |
| `ProjectSettingsTab.tsx` | Settings save/create/reset |
| `KnowledgeStatus.tsx` | Service control, indexing, bootstrap |
| `ActionRow.tsx` | Project action run failure |
| `SettingsPage.tsx` | Config save failure |
| `useRealtimeUpdates.ts` | WorktreeError event |

### Places MISSING Notifications

**Critical - needs user action (action toasts):**
- Tool call pending (approve/reject) - currently only visible on AgenticLoopPanel page
- Loop waiting_for_input - currently only visible on AgenticOverlay
- Permission requests from hooks system

**Important - server events (info toasts):**
- `host_connected` / `host_disconnected` - silent
- `session_suspended` / `session_resumed` - silent
- `agentic_loop_detected` - silent
- `agentic_loop_ended` - silent (especially with errors)
- `claude_task_started` / `claude_task_ended` - silent
- `knowledge_status_changed` - silent

**API error paths (error toasts):**
- Permission rule upsert/delete - no success/failure toast
- Worktree create/delete - no toast
- Project add/delete - only inline dialog error, no toast
- Project git refresh - no toast
- Claude task create/resume - no toast
- Knowledge memory operations - no toast
- All fetch errors in hooks (useSessions, useHosts, useAgenticLoops) - silent console.error

## Architecture

```
                          ┌─────────────────────────┐
                          │   notification-store.ts  │
                          │   (Zustand)              │
                          │                          │
                          │  actionNotifications[]   │  ← persistent, need user response
                          │  enabled: boolean        │
                          │  browserPermission       │
                          └────────┬────────────────-┘
                                   │
              ┌────────────────────┼───────────────────┐
              │                    │                    │
              ▼                    ▼                    ▼
   ┌──────────────────┐  ┌────────────────┐  ┌────────────────────┐
   │  ActionToast.tsx  │  │  showToast()   │  │  browser-notifs.ts │
   │  (persistent,     │  │  (existing,    │  │  (Web Notif API,   │
   │   with buttons)   │  │   auto-dismiss)│  │   tab hidden only) │
   └──────────────────┘  └────────────────┘  └────────────────────┘

Triggers:
  useRealtimeUpdates.ts  ─── WebSocket events ──→  notification-store + showToast
  API call sites         ─── success/error ────→  showToast (existing pattern)
```

## Design Decisions

### D1: Separate ActionToast vs extending Toast
The existing Toast is fire-and-forget with auto-dismiss. Action toasts are fundamentally different: persistent, interactive, need state sync. **Separate component**, rendered alongside ToastContainer in AppLayout.

### D2: Dedup action toasts at loop level
One action toast per loop. Multiple pending tool calls show count ("3 tool calls pending") with latest tool name. Avoids flooding.

### D3: Info toasts for server events go through existing showToast()
No need for a new component - extend usage of the existing `showToast()` for events like host disconnect, session suspend, etc. This keeps the system simple.

### D4: Browser notifications only for action-required events
Browser notifications (OS-level) only for events that need user response: pending tool calls, waiting_for_input. Info events only show in-app toasts, not OS notifications.

### D5: Notification enabled by default
Action toasts and info toasts should work without Settings toggle. The `notifications.enabled` setting controls browser (OS) notifications only. In-app toasts always work.

## Technical Design

### New Files

#### 1. `web/src/lib/browser-notifications.ts`
```typescript
export function isBrowserNotificationSupported(): boolean;
export function getBrowserPermission(): NotificationPermission | "unsupported";
export async function requestBrowserPermission(): Promise<NotificationPermission>;
export function showBrowserNotification(title: string, options: {
  body: string;
  tag: string;       // dedup key
  onClick?: () => void;
}): void;  // only fires when document.visibilityState === "hidden"
```

#### 2. `web/src/stores/notification-store.ts`
```typescript
interface ActionNotification {
  id: string;               // = loop_id
  loopId: string;
  sessionId: string;
  hostId: string;
  hostname: string;
  toolName: string;
  status: "waiting_for_input" | "tool_pending";
  pendingToolCount: number;
  latestToolName: string | null;
  createdAt: number;
}

interface NotificationState {
  notifications: Map<string, ActionNotification>;
  browserPermission: NotificationPermission | "unsupported";
  browserEnabled: boolean;  // from config, controls OS notifications

  addOrUpdate: (notification: ActionNotification) => void;
  dismiss: (loopId: string) => void;
  dismissAll: () => void;
  setBrowserEnabled: (enabled: boolean) => void;
  requestBrowserPermission: () => Promise<NotificationPermission>;
  handleLoopResolved: (loopId: string) => void;
  handleToolResolved: (loopId: string) => void;
}
```

#### 3. `web/src/components/layout/ActionToast.tsx`
Position: `fixed right-4 bottom-20 z-50` (above ToastContainer).
Visual: `bg-bg-secondary border-l-4 border-l-status-warning rounded-lg shadow-lg`, 300-400px width.
Max 3 visible, "+N more" overflow indicator.

Per notification:
- AlertCircle icon with pulse
- Title: tool name or "N tool calls pending"
- Hostname subtitle (server mode)
- Buttons: Approve (Check, green), Reject (X, red), Go to Terminal (Terminal icon)
- Dismiss X button
- `role="alert"`, `aria-label` on all buttons, focus rings

#### 4. Tests
- `web/src/lib/browser-notifications.test.ts`
- `web/src/stores/notification-store.test.ts`
- `web/src/components/layout/ActionToast.test.tsx`

### Modified Files

#### 5. `web/src/hooks/useRealtimeUpdates.ts`
Add `hostname?: string` to ServerEvent interface.

New notification triggers in event handler:

```
agentic_loop_state_update (waiting_for_input)
  → notificationStore.addOrUpdate(...)
  → showBrowserNotification("Claude needs input", ...)

agentic_loop_state_update (working/completed/error)
  → notificationStore.handleLoopResolved(loopId)

agentic_loop_tool_call (pending)
  → notificationStore.addOrUpdate(...) // updates count

agentic_loop_tool_result (non-pending)
  → notificationStore.handleToolResolved(loopId)

agentic_loop_ended
  → notificationStore.handleLoopResolved(loopId)
  → showToast("Loop ended: {reason}", reason === "error" ? "error" : "info")

host_connected
  → showToast("Host {hostname} connected", "success")

host_disconnected
  → showToast("Host {hostname} disconnected", "error")

session_suspended
  → showToast("Session suspended - agent reconnecting", "info")

session_resumed
  → showToast("Session resumed", "success")

claude_task_started
  → showToast("Claude task started", "info")

claude_task_ended
  → showToast("Claude task {status}", status === "completed" ? "success" : "error")
```

#### 6. `web/src/components/layout/AppLayout.tsx`
- Mount `<ActionToastContainer />` before `<ToastContainer />`
- Add useEffect to init browser notification state from config

#### 7. `web/src/components/settings/SettingsPage.tsx`
- Wire `notifications.enabled` toggle to `requestBrowserPermission()` on enable
- Update description: "Enable browser (OS) notifications when Claude needs input"

#### 8. Additional toast coverage (existing pattern, add showToast calls)

**`web/src/components/agentic/AgenticOverlay.tsx`:**
- Approve/reject action success toast (currently only failure)

**`web/src/pages/HostPage.tsx`:**
- Session create success toast

**Permission operations** (wherever `api.permissions.upsert/delete` are called):
- Success/failure toasts

**Worktree operations** (wherever `api.projects.createWorktree/deleteWorktree` are called):
- Success/failure toasts

**Claude task operations** (wherever `api.claudeTasks.create/resume` are called):
- Failure toasts (success covered by server event)

**Knowledge memory operations** (wherever memory CRUD is called):
- Success/failure toasts for update/delete

## Bidirectional Sync

Action toast → user clicks Approve → `sendAction()` + `dismiss(loopId)`
ToolCallQueue/AgenticOverlay → user acts → server event → `handleLoopResolved()` auto-dismisses

Both paths converge on removing the notification from the store. No race conditions.

## Task List

### Phase 1: Core Infrastructure
- [ ] **T1.1** Create `web/src/lib/browser-notifications.ts` - Web Notifications API wrapper
- [ ] **T1.2** Create `web/src/lib/browser-notifications.test.ts` - Tests for browser notifications util
- [ ] **T1.3** Create `web/src/stores/notification-store.ts` - Zustand store for action notifications
- [ ] **T1.4** Create `web/src/stores/notification-store.test.ts` - Tests for notification store

### Phase 2: Action Toast Component
- [ ] **T2.1** Create `web/src/components/layout/ActionToast.tsx` - Persistent action toast with Approve/Reject/Navigate buttons
- [ ] **T2.2** Create `web/src/components/layout/ActionToast.test.tsx` - Tests for ActionToast component
- [ ] **T2.3** Modify `web/src/components/layout/AppLayout.tsx` - Mount ActionToastContainer + init config

### Phase 3: Wire Real-time Events
- [ ] **T3.1** Modify `web/src/hooks/useRealtimeUpdates.ts` - Add action notification triggers for agentic events
- [ ] **T3.2** Modify `web/src/hooks/useRealtimeUpdates.ts` - Add info toasts for host/session/task events
- [ ] **T3.3** Modify `web/src/hooks/useRealtimeUpdates.ts` - Add browser notification triggers (tab hidden)

### Phase 4: Settings Integration
- [ ] **T4.1** Modify `web/src/components/settings/SettingsPage.tsx` - Wire notifications.enabled to browser permission + store

### Phase 5: Expand Toast Coverage (existing showToast pattern)
- [ ] **T5.1** Add success toasts to agentic actions (AgenticOverlay approve/reject)
- [ ] **T5.2** Add success toast to session creation (HostPage)
- [ ] **T5.3** Add success/failure toasts to permission operations
- [ ] **T5.4** Add success/failure toasts to worktree operations
- [ ] **T5.5** Add failure toasts to Claude task create/resume
- [ ] **T5.6** Add success/failure toasts to knowledge memory update/delete

### Phase 6: Verification
- [ ] **T6.1** Run `cd web && bun run typecheck` - no errors
- [ ] **T6.2** Run `cd web && bun run test` - all tests pass

## Key Files Reference

| File | Purpose | Action |
|------|---------|--------|
| `web/src/lib/browser-notifications.ts` | Web Notifications API wrapper | CREATE |
| `web/src/stores/notification-store.ts` | Action notification state | CREATE |
| `web/src/components/layout/ActionToast.tsx` | Persistent action toast UI | CREATE |
| `web/src/components/layout/Toast.tsx` | Existing toast (reuse showToast) | UNCHANGED |
| `web/src/hooks/useRealtimeUpdates.ts` | WebSocket event handler | MODIFY |
| `web/src/components/layout/AppLayout.tsx` | Layout, mount point | MODIFY |
| `web/src/components/settings/SettingsPage.tsx` | Settings toggle | MODIFY |
| `web/src/stores/agentic-store.ts` | sendAction() - reuse | UNCHANGED |
| `web/src/types/agentic.ts` | AgenticLoop, ToolCall types | UNCHANGED |
| `web/src/components/agentic/AgenticOverlay.tsx` | Add success toasts | MODIFY |
| `web/src/pages/HostPage.tsx` | Add session create success | MODIFY |
| `web/src/lib/api.ts` | API client - reuse | UNCHANGED |

## Reusable Existing Code
- `showToast()` from `web/src/components/layout/Toast.tsx` - all info/error/success notifications
- `useAgenticStore.sendAction()` from `web/src/stores/agentic-store.ts` - approve/reject actions
- `IconButton` from `web/src/components/ui/IconButton.tsx` - action buttons in toast
- `Button` from `web/src/components/ui/Button.tsx` - variants: primary, secondary, ghost, danger
- `Badge` from `web/src/components/ui/Badge.tsx` - status badges
- CSS theme tokens from `web/src/index.css` - status-warning, status-online, status-error, etc.
- `useNavigate()` from react-router - Go to Terminal navigation
- Route pattern: `/hosts/:hostId/sessions/:sessionId` for terminal navigation

## Implementation Order

1. T1.1 + T1.2 (browser-notifications, standalone)
2. T1.3 + T1.4 (notification-store, depends on #1)
3. T2.1 + T2.2 (ActionToast component, depends on #2)
4. T2.3 (mount in AppLayout)
5. T3.1 + T3.2 + T3.3 (wire events in useRealtimeUpdates)
6. T4.1 (settings integration)
7. T5.1-T5.6 (expand toast coverage, independent of each other)
8. T6.1-T6.2 (verification)
