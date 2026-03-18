import { ArrowLeft, Bot, Pencil, SquareTerminal, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { useHosts } from "../hooks/useHosts";
import { useSessions } from "../hooks/useSessions";
import { useAgenticLoops } from "../hooks/useAgenticLoops";
import { useClaudeTaskStore } from "../stores/claude-task-store";
import { useSessionMruStore } from "../stores/session-mru-store";
import { api } from "../lib/api";
import { Badge } from "../components/ui/Badge";
import { IconButton } from "../components/ui/IconButton";
import { Terminal } from "../components/Terminal";
import { PaneTabBar } from "../components/PaneTabBar";
import { AgenticOverlay } from "../components/agentic/AgenticOverlay";
import { showToast } from "../components/layout/Toast";
import type { PaneInfo, PaneEvent } from "../types/terminal";

export function SessionPage() {
  const { hostId, sessionId } = useParams<{
    hostId: string;
    sessionId: string;
  }>();
  const navigate = useNavigate();
  const { hosts } = useHosts();
  const { sessions, refetch } = useSessions(hostId);
  const [closing, setClosing] = useState(false);
  const [editingName, setEditingName] = useState(false);
  const [nameValue, setNameValue] = useState("");
  const nameInputRef = useRef<HTMLInputElement>(null);
  const [panes, setPanes] = useState<PaneInfo[]>([]);
  const [activePaneId, setActivePaneId] = useState<string | undefined>(undefined);

  const host = hosts.find((h) => h.id === hostId);
  const session = sessions.find((s) => s.id === sessionId);

  // Check if this session is a Claude task
  const claudeTaskId = useClaudeTaskStore(
    (s) => (sessionId ? s.sessionTaskIndex.get(sessionId) : undefined),
  );
  const claudeTask = useClaudeTaskStore((s) =>
    claudeTaskId ? s.tasks.get(claudeTaskId) : undefined,
  );

  // Fetch claude tasks for this host to populate the index
  useEffect(() => {
    if (hostId) {
      void useClaudeTaskStore.getState().fetchTasks({ host_id: hostId });
    }
  }, [hostId]);

  // Record MRU visit when session is viewed
  const recordVisit = useSessionMruStore((s) => s.recordVisit);
  useEffect(() => {
    if (sessionId) recordVisit(sessionId);
  }, [sessionId, recordVisit]);

  // Get active agentic loop for this session
  const { loops } = useAgenticLoops(
    session?.status === "active" ? sessionId : undefined,
  );
  const activeLoop = useMemo(
    () =>
      loops.find(
        (l) => l.status !== "completed" && l.status !== "error",
      ),
    [loops],
  );

  // Determine the loop to show in overlay
  const overlayLoopId = claudeTask?.loop_id ?? activeLoop?.id;

  const handleCopyTmuxCommand = useCallback(() => {
    if (!session?.tmux_name) return;
    const command = `tmux -L zremote attach-session -t ${session.tmux_name}`;
    void navigator.clipboard.writeText(command).then(
      () => showToast("Tmux attach command copied", "success"),
      () => showToast("Failed to copy to clipboard", "error"),
    );
  }, [session?.tmux_name]);

  const handleClose = useCallback(async () => {
    if (!hostId || !sessionId || closing) return;
    if (!window.confirm("Close this session?")) return;
    setClosing(true);
    try {
      await api.sessions.close(sessionId);
      showToast("Session closed", "success");
      void navigate(`/hosts/${hostId}`);
    } catch {
      showToast("Failed to close session", "error");
      setClosing(false);
    }
  }, [hostId, sessionId, closing, navigate]);

  const handleStartEditing = useCallback(() => {
    if (!session) return;
    setNameValue(session.name ?? "");
    setEditingName(true);
    setTimeout(() => nameInputRef.current?.focus(), 50);
  }, [session]);

  const handleRenameSubmit = useCallback(async () => {
    if (!sessionId) return;
    setEditingName(false);
    const trimmed = nameValue.trim();
    try {
      await api.sessions.rename(sessionId, trimmed || null);
      void refetch();
      showToast("Session renamed", "success");
    } catch (e) {
      console.error("failed to rename session", e);
      showToast("Failed to rename session", "error");
    }
  }, [sessionId, nameValue, refetch]);

  const handlePaneEvent = useCallback((event: PaneEvent) => {
    if (event.type === "pane_added") {
      setPanes((prev) => {
        if (prev.some((p) => p.pane_id === event.pane_id)) return prev;
        return [...prev, { pane_id: event.pane_id, index: event.index }];
      });
    } else if (event.type === "pane_removed") {
      setPanes((prev) => prev.filter((p) => p.pane_id !== event.pane_id));
      setActivePaneId((prev) =>
        prev === event.pane_id ? undefined : prev,
      );
    }
  }, []);

  const handleRenameKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        void handleRenameSubmit();
      } else if (e.key === "Escape") {
        setEditingName(false);
      }
    },
    [handleRenameSubmit],
  );

  if (!session) {
    return (
      <div className="flex h-full items-center justify-center text-text-secondary">
        Session not found
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      {/* Session header */}
      <div className="flex items-center gap-3 border-b border-border px-4 py-2">
        <Link
          to={`/hosts/${hostId}`}
          className="text-text-tertiary transition-colors duration-150 hover:text-text-primary"
        >
          <ArrowLeft size={16} />
        </Link>
        {!!claudeTask && (
          <Bot size={14} className="shrink-0 text-accent" />
        )}
        {claudeTask?.task_name && (
          <span className="text-sm font-medium text-accent">
            {claudeTask.task_name}
          </span>
        )}
        <span className="font-mono text-xs text-text-tertiary">
          {sessionId?.slice(0, 8)}
        </span>
        {editingName ? (
          <input
            ref={nameInputRef}
            value={nameValue}
            onChange={(e) => setNameValue(e.target.value)}
            onBlur={() => void handleRenameSubmit()}
            onKeyDown={handleRenameKeyDown}
            className="h-6 rounded border border-accent bg-bg-tertiary px-2 text-sm text-text-primary focus:ring-2 focus:ring-accent/20 focus:outline-none"
            placeholder="Session name"
          />
        ) : (
          <>
            <span className="text-sm text-text-primary">
              {session.name || session.shell || "shell"}
            </span>
            <button
              onClick={handleStartEditing}
              className="text-text-tertiary transition-colors duration-150 hover:text-text-primary"
              title="Rename session"
            >
              <Pencil size={12} />
            </button>
          </>
        )}
        {host && (
          <Link
            to={`/hosts/${hostId}`}
            className="text-xs text-text-tertiary transition-colors duration-150 hover:text-accent"
          >
            {host.hostname}
          </Link>
        )}
        <Badge
          variant={
            session.status === "active"
              ? "online"
              : session.status === "error"
                ? "error"
                : session.status === "creating"
                  ? "creating"
                  : "offline"
          }
        >
          {session.status}
        </Badge>
        <div className="ml-auto flex items-center gap-1">
          {session.tmux_name && (session.status === "active" || session.status === "suspended") && (
            <IconButton
              icon={SquareTerminal}
              tooltip={`Copy: tmux -L zremote attach-session -t ${session.tmux_name}`}
              onClick={handleCopyTmuxCommand}
              aria-label="Copy tmux attach command"
            />
          )}
          <IconButton
            icon={X}
            tooltip="Close session"
            onClick={() => void handleClose()}
            disabled={closing || session.status === "closed"}
          />
        </div>
      </div>

      {/* Terminal area with optional overlay */}
      <div className="relative min-h-0 flex-1 flex flex-col">
        {overlayLoopId && <AgenticOverlay loopId={overlayLoopId} />}
        <PaneTabBar
          panes={panes}
          activePaneId={activePaneId}
          onSelectPane={setActivePaneId}
        />
        <div className="relative min-h-0 flex-1">
          {sessionId && (
            <>
              {/* Main terminal - always mounted, hidden when not active */}
              <div
                className="absolute inset-0"
                style={{ display: activePaneId === undefined ? "block" : "none" }}
              >
                <Terminal
                  sessionId={sessionId}
                  onPaneEvent={handlePaneEvent}
                />
              </div>
              {/* Extra pane terminals - always mounted, hidden when not active */}
              {panes.map((pane) => (
                <div
                  key={pane.pane_id}
                  className="absolute inset-0"
                  style={{ display: activePaneId === pane.pane_id ? "block" : "none" }}
                >
                  <Terminal
                    sessionId={sessionId}
                    paneId={pane.pane_id}
                  />
                </div>
              ))}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
