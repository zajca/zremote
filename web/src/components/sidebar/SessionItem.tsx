import { Bot, Pause, Terminal, X } from "lucide-react";
import { memo, useCallback } from "react";
import { useLocation, useNavigate } from "react-router";
import type { Session } from "../../lib/api";
import { api } from "../../lib/api";
import { useAgenticLoops } from "../../hooks/useAgenticLoops";
import { useClaudeTaskStore } from "../../stores/claude-task-store";
import { SESSION_UPDATE_EVENT } from "../../hooks/useSessions";
import { Badge } from "../ui/Badge";
import { showToast } from "../layout/Toast";

interface SessionItemProps {
  session: Session;
  hostId: string;
}

function sessionStatusVariant(
  status: Session["status"],
): "online" | "offline" | "error" | "warning" | "creating" {
  switch (status) {
    case "active":
      return "online";
    case "closed":
      return "offline";
    case "error":
      return "error";
    case "creating":
      return "creating";
    case "suspended":
      return "warning";
    default:
      return "offline";
  }
}

export const SessionItem = memo(function SessionItem({
  session,
  hostId,
}: SessionItemProps) {
  const navigate = useNavigate();
  const location = useLocation();
  const isActive = location.pathname.includes(`/sessions/${session.id}`);
  const isClaudeTask = useClaudeTaskStore(
    (s) => s.sessionTaskIndex.has(session.id),
  );
  const { loops } = useAgenticLoops(
    session.status === "active" || session.status === "suspended" ? session.id : undefined,
  );

  const activeLoops = loops.filter(
    (l) => l.status !== "completed" && l.status !== "error",
  );
  const waitingLoops = activeLoops.filter(
    (l) => l.status === "waiting_for_input",
  );

  const handleClick = useCallback(() => {
    void navigate(`/hosts/${hostId}/sessions/${session.id}`);
  }, [navigate, hostId, session.id]);

  const handleLoopClick = useCallback(
    (e: React.MouseEvent, loopId: string) => {
      e.stopPropagation();
      void navigate(
        `/hosts/${hostId}/sessions/${session.id}/loops/${loopId}`,
      );
    },
    [navigate, hostId, session.id],
  );

  const handleClose = useCallback(
    async (e: React.MouseEvent) => {
      e.stopPropagation();
      if (!window.confirm("Close this session?")) return;
      try {
        await api.sessions.close(session.id);
        showToast("Session closed", "success");
        window.dispatchEvent(new Event(SESSION_UPDATE_EVENT));
      } catch (err) {
        console.error("failed to close session", err);
        showToast("Failed to close session", "error");
      }
    },
    [session.id],
  );

  return (
    <div>
      <div
        className={`group/session flex h-7 w-full items-center gap-2 px-2 text-[13px] transition-colors duration-150 hover:bg-bg-hover ${isActive ? "bg-bg-hover text-text-primary" : "text-text-secondary"}`}
      >
        <button
          onClick={handleClick}
          className="flex min-w-0 flex-1 items-center gap-2"
        >
          {session.status === "suspended" ? (
            <Pause size={13} className="shrink-0 text-status-warning" />
          ) : isClaudeTask ? (
            <Bot size={13} className="shrink-0 text-accent" />
          ) : (
            <Terminal size={13} className="shrink-0 text-text-tertiary" />
          )}
          <span className="truncate">{session.name || session.shell || "shell"}</span>
          <Badge variant={sessionStatusVariant(session.status)}>
            {session.status}
          </Badge>
          {activeLoops.length > 0 && (
            <span
              className={`ml-auto inline-flex h-4 min-w-[16px] items-center justify-center rounded-full px-1 text-[10px] font-medium ${
                waitingLoops.length > 0
                  ? "animate-pulse bg-status-warning/20 text-status-warning"
                  : "bg-accent/20 text-accent"
              }`}
            >
              <Bot size={10} className="mr-0.5" />
              {activeLoops.length}
            </span>
          )}
        </button>
        {(session.status === "active" || session.status === "suspended") && (
          <button
            onClick={handleClose}
            className="hidden h-4 w-4 shrink-0 items-center justify-center rounded text-text-tertiary transition-colors duration-150 hover:bg-bg-active hover:text-status-error group-hover/session:flex"
            aria-label="Close session"
          >
            <X size={12} />
          </button>
        )}
      </div>
      {activeLoops.map((loop) => (
        <button
          key={loop.id}
          onClick={(e) => handleLoopClick(e, loop.id)}
          className="flex h-6 w-full items-center gap-1.5 pl-7 pr-2 text-[11px] text-text-tertiary transition-colors hover:bg-bg-hover hover:text-text-secondary"
        >
          <Bot size={11} className={loop.status === "waiting_for_input" ? "animate-pulse text-status-warning" : "text-accent"} />
          <span className="truncate">{loop.task_name || loop.tool_name}</span>
          <Badge
            variant={
              loop.status === "waiting_for_input"
                ? "warning"
                : loop.status === "working"
                  ? "creating"
                  : "offline"
            }
          >
            {loop.status}
          </Badge>
          {loop.pending_tool_calls > 0 && (
            <span className="ml-auto rounded bg-status-warning/15 px-1 text-[10px] text-status-warning">
              {loop.pending_tool_calls}
            </span>
          )}
        </button>
      ))}
    </div>
  );
});
