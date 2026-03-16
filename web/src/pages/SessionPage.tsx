import { ArrowLeft, Bot, Columns2, Maximize2, Pencil, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { useHosts } from "../hooks/useHosts";
import { useSessions } from "../hooks/useSessions";
import { useAgenticLoops } from "../hooks/useAgenticLoops";
import { useClaudeTaskStore } from "../stores/claude-task-store";
import { api } from "../lib/api";
import { Badge } from "../components/ui/Badge";
import { IconButton } from "../components/ui/IconButton";
import { Terminal } from "../components/Terminal";
import { AgenticLoopPanel } from "../components/agentic/AgenticLoopPanel";

const MIN_PANEL_PCT = 20;
const MAX_PANEL_PCT = 80;
const DEFAULT_TERMINAL_PCT = 40;

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

  // Determine the loop to show in split view
  const splitLoopId = claudeTask?.loop_id ?? activeLoop?.id;
  const isClaudeSession = !!claudeTask;

  // Split view state
  const [splitActive, setSplitActive] = useState(false);
  const [terminalPct, setTerminalPct] = useState(DEFAULT_TERMINAL_PCT);
  const containerRef = useRef<HTMLDivElement>(null);
  const dragging = useRef(false);

  // Auto-enable split when Claude session has a loop
  useEffect(() => {
    if (isClaudeSession && splitLoopId) {
      setSplitActive(true);
    }
  }, [isClaudeSession, splitLoopId]);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    dragging.current = true;

    const onMouseMove = (ev: MouseEvent) => {
      if (!dragging.current || !containerRef.current) return;
      const rect = containerRef.current.getBoundingClientRect();
      const pct = ((ev.clientX - rect.left) / rect.width) * 100;
      setTerminalPct(Math.min(MAX_PANEL_PCT, Math.max(MIN_PANEL_PCT, pct)));
    };

    const onMouseUp = () => {
      dragging.current = false;
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
    };

    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);
  }, []);

  const handleClose = useCallback(async () => {
    if (!hostId || !sessionId || closing) return;
    if (!window.confirm("Close this session?")) return;
    setClosing(true);
    try {
      await api.sessions.close(sessionId);
      void navigate(`/hosts/${hostId}`);
    } catch {
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
    } catch (e) {
      console.error("failed to rename session", e);
    }
  }, [sessionId, nameValue, refetch]);

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
      <div className="flex items-center gap-3 border-b border-border px-4 py-2">
        <Link
          to={`/hosts/${hostId}`}
          className="text-text-tertiary transition-colors duration-150 hover:text-text-primary"
        >
          <ArrowLeft size={16} />
        </Link>
        {isClaudeSession && (
          <Bot size={14} className="shrink-0 text-accent" />
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
          {splitLoopId && (
            <IconButton
              icon={splitActive ? Maximize2 : Columns2}
              tooltip={splitActive ? "Full-width terminal" : "Show split view"}
              onClick={() => setSplitActive((prev) => !prev)}
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

      <div ref={containerRef} className="flex min-h-0 flex-1">
        {splitActive && splitLoopId ? (
          <>
            {/* Terminal panel */}
            <div
              className="min-h-0 min-w-0 overflow-hidden"
              style={{ width: `${terminalPct}%` }}
            >
              {sessionId && <Terminal sessionId={sessionId} />}
            </div>

            {/* Drag handle */}
            <div
              onMouseDown={handleMouseDown}
              className="flex w-1.5 shrink-0 cursor-col-resize items-center justify-center bg-border transition-colors hover:bg-accent/50"
            >
              <div className="h-8 w-0.5 rounded-full bg-text-tertiary/30" />
            </div>

            {/* Agentic panel */}
            <div
              className="min-h-0 min-w-0 overflow-hidden"
              style={{ width: `${100 - terminalPct}%` }}
            >
              <AgenticLoopPanel loopId={splitLoopId} />
            </div>
          </>
        ) : (
          <div className="min-h-0 flex-1">
            {sessionId && <Terminal sessionId={sessionId} />}
          </div>
        )}
      </div>
    </div>
  );
}
