# Phase 3: Web UI Foundation (Linear-inspired)

**Goal:** Build the core web UI shell -- design system, layout, sidebar navigation, real-time status updates, and basic pages for hosts and sessions.

**Dependencies:** Phase 1 (REST API), Phase 2 (Terminal component)

---

## 3.1 Design System

**Files:** `web/src/index.css`, `web/src/components/ui/{Button,Badge,StatusDot,IconButton,Tooltip,Input}.tsx`

- [ ] Define CSS custom properties in `index.css` using Tailwind 4 `@theme` directive:
  - Backgrounds: `--color-bg-primary` (#0a0a0b), `--color-bg-secondary` (#111113), `--color-bg-tertiary` (#1a1a1e), `--color-bg-hover` (#222228), `--color-bg-active` (#2a2a32)
  - Text: `--color-text-primary` (#f0f0f3), `--color-text-secondary` (#8b8b93), `--color-text-tertiary` (#5c5c66)
  - Accent: `--color-accent` (#5e6ad2), `--color-accent-hover` (#6e7ae2)
  - Status: `--color-status-online` (#4ade80), `--color-status-offline` (#6b7280), `--color-status-error` (#ef4444), `--color-status-warning` (#f59e0b)
  - Borders: `--color-border` (#222228), `--color-border-hover` (#333340)
  - Spacing scale: 4/8/12/16/24px
- [ ] Typography: `Inter` for UI (13px sidebar, 14px content), `JetBrains Mono` for terminal/code -- add via `@fontsource/inter` and `@fontsource/jetbrains-mono` npm packages
- [ ] `Button` component:
  - Variants: `primary` (accent bg), `secondary` (transparent, border), `ghost` (no border), `danger` (red)
  - Sizes: `sm`, `md`
  - Proper focus ring, disabled state, 150ms hover transition
- [ ] `Badge` component: status text with colored background (e.g., "online" green, "offline" gray, "error" red)
- [ ] `StatusDot` component: 8px circle, color based on status prop, optional pulse animation
- [ ] `IconButton` component: square button with icon, tooltip on hover
- [ ] `Tooltip` component: simple CSS tooltip on hover, dark bg, small text
- [ ] `Input` component: text input with dark bg, border, focus ring, label support

---

## 3.2 Layout & Routing

**Files:** `web/src/components/layout/{AppLayout,Sidebar,MainContent}.tsx`, `web/src/App.tsx`

- [ ] Install npm packages: `react-router` (v7), `lucide-react` (icons)
- [ ] Create `AppLayout` component:
  - Flex row, full viewport height (`h-screen`)
  - Left: `Sidebar` (256px fixed width, bg-secondary, border-right)
  - Right: `<Outlet />` (flex-1, bg-primary, overflow-auto)
- [ ] Define routes in `App.tsx`:
  - `/` -> `WelcomePage`
  - `/hosts/:hostId` -> `HostPage`
  - `/hosts/:hostId/sessions/:sessionId` -> `SessionPage`
  - `/settings` -> placeholder
  - All wrapped in `AppLayout`
- [ ] `BrowserRouter` setup with `createBrowserRouter`

---

## 3.3 Sidebar

**Files:** `web/src/components/sidebar/{Sidebar,HostItem,SessionItem}.tsx`, `web/src/hooks/{useHosts,useSessions}.ts`

- [ ] `useHosts()` hook:
  - Fetch `GET /api/hosts` on mount
  - Return `{ hosts, loading, error, refetch }`
  - Auto-refetch on real-time events (Phase 3.4)
- [ ] `useSessions(hostId)` hook:
  - Fetch `GET /api/hosts/{hostId}/sessions` on mount + hostId change
  - Return `{ sessions, loading, error, refetch }`
- [ ] `Sidebar` component:
  - Header: "MyRemote" branding, version
  - Host list with `HostItem` components
  - Bottom: settings link
- [ ] `HostItem` component:
  - Status dot + hostname + session count badge
  - Expandable/collapsible (click to toggle)
  - When expanded: list of `SessionItem` children + "New Session" button
  - Active state highlighting when selected (matches current route)
  - 32px height, truncate long names with CSS ellipsis
  - `React.memo` for performance
- [ ] `SessionItem` component:
  - Shell name + status badge
  - Click navigates to `/hosts/{hostId}/sessions/{sessionId}`
  - Active state highlighting
  - `React.memo` for performance
- [ ] Expand/collapse state persisted in `localStorage`
- [ ] Hover states with 150ms ease transition

---

## 3.4 Real-time Status Updates

**Files:** `crates/myremote-server/src/routes/events.rs`, `web/src/hooks/useRealtimeUpdates.ts`

- [ ] Server: `/ws/events` endpoint
  - Use `tokio::sync::broadcast` channel (buffer 1024) for server-wide events
  - On browser connect: send current state snapshot, then stream events
  - Handle `RecvError::Lagged` by sending full state refresh
- [ ] Event types (JSON):
  - `{ "type": "host_connected", "host": {...} }`
  - `{ "type": "host_disconnected", "host_id": "..." }`
  - `{ "type": "host_status_changed", "host_id": "...", "status": "..." }`
  - `{ "type": "session_created", "session": {...} }`
  - `{ "type": "session_closed", "session_id": "...", "exit_code": ... }`
- [ ] Emit events from: agent WS handler (connect/disconnect/heartbeat timeout), session endpoints (create/close)
- [ ] `useRealtimeUpdates` hook (browser):
  - Connect to `/ws/events` via `useWebSocket`
  - Parse events, call provided callbacks: `onHostUpdate`, `onSessionUpdate`
  - Auto-reconnect on disconnect
  - On reconnect: re-fetch full state to ensure consistency
  - Cleanup subscriptions in useEffect return

---

## 3.5 Pages

**Files:** `web/src/pages/{WelcomePage,HostPage,SessionPage}.tsx`

- [ ] `WelcomePage`:
  - Centered content, MyRemote logo/title
  - Brief description of the tool
  - Instructions for connecting first agent (show example env vars + command)
  - Empty state when no hosts connected -- clear call-to-action
- [ ] `HostPage`:
  - Header: host name (editable inline), status dot, OS/arch, agent version, last seen
  - Session list table: id (truncated), shell, status badge, created_at, actions (close button)
  - "New Session" button -> calls `POST /api/hosts/{hostId}/sessions` with defaults (80x24), navigates to session page
  - Empty state when no sessions: "No active sessions" + "Start Session" button
- [ ] `SessionPage`:
  - Header bar: session ID (truncated), shell name, host name (link), status badge, close button (with confirm)
  - Full-height `Terminal` component below header (flex-1)
  - When session is closed: overlay message with exit code

---

## 3.6 Keyboard Navigation Foundation

**Files:** `web/src/hooks/useKeyboardShortcuts.ts`

- [ ] Global keyboard shortcut handler via `useEffect` on `document`
- [ ] `Cmd/Ctrl+K` -> opens command palette stub (empty modal for now, will be extended in Phase 7)
- [ ] Sidebar keyboard navigation:
  - Arrow Up/Down to move between items
  - Enter to select/navigate
  - Escape to deselect
  - Left/Right to collapse/expand host items
- [ ] Tab focus management: Tab moves focus between sidebar and main content
- [ ] Ensure terminal captures keyboard when focused (don't intercept terminal input)

---

## Verification Checklist

1. [ ] Open browser -> see sidebar with connected hosts and their sessions
2. [ ] Click host -> expand to show sessions + "New Session" button
3. [ ] Click session -> navigate to terminal view -> terminal renders correctly
4. [ ] Create new session from HostPage -> terminal opens
5. [ ] Agent goes offline -> sidebar updates status dot in real-time (no page refresh)
6. [ ] New session created from another tab -> appears in sidebar in real-time
7. [ ] Resize browser -> terminal auto-fits
8. [ ] Keyboard: Ctrl+K opens command palette stub
9. [ ] No hosts connected -> WelcomePage with instructions shown

## Review Notes

- React re-renders: memoize HostItem, SessionItem with React.memo
- WebSocket reconnect in useRealtimeUpdates -- must not leak connections
- Cleanup all event listeners and subscriptions in useEffect return
- TypeScript strict mode: no `any`, proper typing for all API responses
- Transitions: 150ms ease for hover/active (Linear uses subtle, fast transitions)
- Font sizes: 13px sidebar items (Linear-like density), 14px main content
- broadcast channel with large buffer (1024) + handle RecvError::Lagged by re-fetching state
- No state management library yet (useState + hooks) -- sufficient for Phase 3
