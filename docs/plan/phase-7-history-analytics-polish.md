# Phase 7: History, Analytics & Polish

**Goal:** Add analytics dashboard, history browser with full-text search, credential management UI, swarm/team visualization, performance optimization, command palette, and error handling polish.

**Dependencies:** All previous phases

---

## 7.1 History & Analytics Data

**Files:** `crates/myremote-server/src/analytics/queries.rs`, `routes/analytics.rs`, `routes/search.rs`, `migrations/004_analytics.sql`

- [ ] `004_analytics.sql` migration:
  ```sql
  CREATE TABLE session_stats (
      session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
      total_bytes_in INTEGER DEFAULT 0,
      total_bytes_out INTEGER DEFAULT 0,
      total_commands INTEGER DEFAULT 0,
      duration_seconds INTEGER DEFAULT 0
  );

  -- Full-text search for transcripts
  CREATE VIRTUAL TABLE transcript_fts USING fts5(
      content,
      content='transcript_entries',
      content_rowid='id'
  );

  -- Triggers to keep FTS in sync
  CREATE TRIGGER transcript_fts_insert AFTER INSERT ON transcript_entries BEGIN
      INSERT INTO transcript_fts(rowid, content) VALUES (new.id, new.content);
  END;

  CREATE TRIGGER transcript_fts_delete AFTER DELETE ON transcript_entries BEGIN
      INSERT INTO transcript_fts(transcript_fts, rowid, content) VALUES ('delete', old.id, old.content);
  END;
  ```
- [ ] Aggregation queries:
  - Token usage by day/model/host/project (GROUP BY with date formatting)
  - Cost by period (day/week/month) -- sum estimated_cost_usd
  - Session stats: count, avg duration, total by host
  - Loop stats: count by status, avg duration, avg cost
  - Run analytics queries on read-only connection or in background task with cached results
- [ ] REST API:
  - `GET /api/analytics/tokens?by=day|model|host|project&from=&to=` -- token usage breakdown
  - `GET /api/analytics/cost?granularity=day|week|month&from=&to=` -- cost over time
  - `GET /api/analytics/sessions?from=&to=` -- session statistics
  - `GET /api/analytics/loops?from=&to=` -- loop statistics
  - `GET /api/search/transcripts?q=&host=&project=&from=&to=` -- FTS search with filters, pagination

---

## 7.2 Frontend: Analytics Dashboard

**Files:** `web/src/components/analytics/{AnalyticsDashboard,StatCard,Charts}.tsx`

- [ ] Install npm packages: `recharts`, `date-fns`
- [ ] Route: `/analytics`
- [ ] `AnalyticsDashboard` page:
  - Date range picker: Today, 7d, 30d, 90d, Custom range
  - Stat cards grid (2x2 or responsive):
    - Total Cost (with trend arrow vs previous period)
    - Total Tokens (in + out)
    - Active Sessions (current)
    - Loops Completed (in period)
  - `StatCard` component: large number, label, trend indicator (up/down arrow + percentage), subtle bg color
- [ ] Charts (recharts):
  - Cost over time (line chart, area fill)
  - Token usage by model (stacked bar or donut)
  - Usage by host (horizontal bar)
  - Usage by project (horizontal bar)
  - Dark theme: muted grid lines, bright data colors on dark bg
  - No 3D effects, no excessive animation
  - Responsive: stack charts vertically on narrow screens
- [ ] Lazy-load charts with `React.lazy()` -- recharts is heavy

---

## 7.3 History Browser

**Files:** `web/src/components/history/HistoryBrowser.tsx`

- [ ] Route: `/history`
- [ ] `HistoryBrowser` page:
  - Search bar: full-text search across transcripts (uses `/api/search/transcripts`)
  - Filters: host dropdown, project dropdown, date range, status dropdown
  - Results list (paginated, 20 per page):
    - Each item: date, host, project, tool name, duration, cost, status badge, summary preview
    - Click opens transcript viewer
  - Transcript viewer: reuse `TranscriptView` component in read-only mode
  - Search results highlight matching terms
  - Empty state: "No loops found" with filter reset button
- [ ] Pagination: infinite scroll or page numbers (start with page numbers for simplicity)
- [ ] Fast filtering: debounce search input (300ms), immediate filter dropdowns

---

## 7.4 Credential Management UI

**Files:** `web/src/components/hosts/CredentialDashboard.tsx`, protocol + agent extensions

- [ ] Protocol extensions:
  - `ServerMessage::CredentialStatusRequest`
  - `AgentMessage::CredentialStatusResponse { oauth_status: Vec<OAuthCredential>, api_keys: Vec<ApiKeyStatus>, mcp_servers: Vec<McpServerStatus> }`
  - Supporting types: `OAuthCredential { provider, status, expires_at }`, `ApiKeyStatus { name, last_four_chars, is_valid }`, `McpServerStatus { name, status }`
- [ ] Agent: check credential status on request
  - Check common credential locations (~/.claude/, env vars, etc.)
  - NEVER send full credentials -- only status, last 4 chars, expiry
- [ ] `CredentialDashboard` component (part of host detail page):
  - Traffic-light indicators per credential type per host (green/yellow/red)
  - Expandable rows:
    - OAuth: status, expiry date, refresh button
    - API keys: name, masked (last 4 chars only), valid/invalid indicator
    - MCP servers: name, connected/disconnected status
  - "Refresh" button to re-check credentials
  - NEVER show full API keys (mask, show last 4 chars only)

---

## 7.5 Swarm/Team Visualization

**Files:** `web/src/components/agentic/TeamTreeView.tsx`, protocol extension, migration

- [ ] Protocol extension:
  - `AgentMessage::AgenticLoopSpawnChild { parent_loop_id: AgenticLoopId, child_loop_id: AgenticLoopId, task: String, role: String }`
  - Add `parent_loop_id: Option<AgenticLoopId>` to `AgenticLoopDetected`
- [ ] DB migration (add to 004_analytics.sql or new 005):
  - Add `parent_loop_id TEXT REFERENCES agentic_loops(id)` column to `agentic_loops`
  - Add `role TEXT` column
- [ ] `TeamTreeView` component:
  - Tree visualization: parent loop at top, children indented below with connecting lines
  - Each node: loop name/task, status dot, role label, tool icon
  - Click child -> navigate to its detail panel
  - Batch actions at parent level: Pause All, Resume All, Stop All (with confirm)
- [ ] Sidebar: nested children under parent loop with visual connecting lines

---

## 7.6 Performance Optimization

- [ ] Terminal output buffering:
  - Batch WS messages per 16ms frame (requestAnimationFrame) instead of per-byte/per-message
  - Reduce re-renders in React terminal component
- [ ] Virtual scrolling for long lists:
  - Install `@tanstack/react-virtual`
  - Apply to: ToolCallQueue (>100 items), TranscriptView (>200 entries), HistoryBrowser results
- [ ] WebSocket reconnection with state recovery:
  - On reconnect, request full state snapshot from server
  - Server sends current hosts, sessions, active loops
- [ ] Code splitting with `React.lazy()`:
  - Separate chunks for: Analytics (recharts), Settings, ClaudeMdEditor (Monaco), HistoryBrowser
  - Preload heavy components on hover (router-level prefetching)
- [ ] Bundle analysis: ensure initial bundle stays under 200KB gzipped

---

## 7.7 Error Handling Polish

- [ ] Server: consistent error response format for all endpoints
  ```json
  { "error": { "code": "HOST_NOT_FOUND", "message": "Host with ID ... not found" } }
  ```
  - Error codes: `HOST_NOT_FOUND`, `SESSION_NOT_FOUND`, `HOST_OFFLINE`, `UNAUTHORIZED`, `BAD_REQUEST`, `INTERNAL_ERROR`
- [ ] Frontend error handling:
  - Global error boundary (catch React rendering errors, show fallback UI)
  - Toast notifications for transient errors: dark card, red left border, auto-dismiss 8s, dismissible
  - Inline error states with retry buttons (for failed API calls in components)
  - "Reconnecting..." banner at top of UI during WS disconnects (fixed position, subtle yellow bg)
- [ ] Agent error handling:
  - Structured error logging (JSON format with error type, context)
  - PTY crash detection: monitor child process, send `SessionClosed` on unexpected exit
  - Agentic tool crash: detect process exit, send `AgenticLoopEnded` with error reason
  - Cleanup: ensure all resources freed on any error path

---

## 7.8 Command Palette

**Files:** `web/src/components/layout/CommandPalette.tsx`

- [ ] Install `cmdk` (Linear-style command palette)
- [ ] `Cmd/Ctrl+K` to open (replace stub from Phase 3.6)
- [ ] Command categories:
  - Navigation: "Go to {host}", "Go to {session}", "Go to {loop}", "Open Analytics", "Open History", "Open Settings"
  - Actions: "Create new session on {host}", "Close session {id}", "Scan projects on {host}"
  - Search: "Search transcripts: {query}" (opens history browser with query)
- [ ] Fuzzy search across all navigable items
- [ ] Keyboard: arrow keys navigate, Enter selects, Esc closes
- [ ] Recent items shown by default when palette opens
- [ ] <100ms to open, fuzzy search feels instant

---

## Verification Checklist

1. [ ] Analytics dashboard shows real data: cost chart, token breakdown, session stats
2. [ ] Date range picker filters analytics correctly
3. [ ] History browser: search "function" -> finds matching transcript entries with highlights
4. [ ] History browser: filter by host + project -> correct results
5. [ ] Click history item -> transcript viewer opens in read-only mode
6. [ ] Credential dashboard shows green/yellow/red indicators per credential
7. [ ] Swarm/team view: parent loop with 2 children -> tree renders correctly
8. [ ] Cmd+K opens command palette -> type host name -> navigate to it
9. [ ] Terminal with 10K lines of output -> no UI lag (virtual scrolling)
10. [ ] WS disconnects -> "Reconnecting..." banner -> reconnects -> banner disappears
11. [ ] API error -> toast notification appears -> auto-dismisses after 8s

## Review Notes

- Analytics queries MUST NOT block main DB -- use read-only connection or background caching
- FTS5 triggers keep index in sync -- add "reindex" management endpoint for recovery
- Credential dashboard NEVER shows full API keys
- Analytics caching: compute daily aggregates in background, serve from cache
- Code splitting keeps initial bundle small
- Swarm visualization is forward-looking -- data model now, minimal UI initially
- Command palette is high-impact UX feature for power users
- SQLite FTS5 sufficient for personal use search
