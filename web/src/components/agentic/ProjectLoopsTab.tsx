import { RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { api } from "../../lib/api";
import type { AgenticLoop, AgenticStatus } from "../../types/agentic";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import { StatusDot } from "../ui/StatusDot";

interface ProjectLoopsTabProps {
  projectId: string;
  hostId: string;
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

function formatRelativeTime(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const seconds = Math.floor(diff / 1000);
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

function formatDuration(start: string, end: string): string {
  const ms = new Date(end).getTime() - new Date(start).getTime();
  const totalSeconds = Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) return `${hours}h ${String(minutes).padStart(2, "0")}m`;
  return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
}

function formatCost(usd: number): string {
  return `$${usd.toFixed(2)}`;
}

function statusDotProps(status: AgenticStatus): {
  status: "online" | "offline" | "error";
  pulse: boolean;
} {
  switch (status) {
    case "working":
      return { status: "online", pulse: true };
    case "waiting_for_input":
      return { status: "online", pulse: false };
    case "paused":
      return { status: "offline", pulse: false };
    case "error":
      return { status: "error", pulse: false };
    case "completed":
      return { status: "online", pulse: false };
  }
}

function statusBadgeVariant(
  status: AgenticStatus,
): "online" | "offline" | "error" | "warning" | "creating" {
  switch (status) {
    case "working":
      return "creating";
    case "waiting_for_input":
      return "warning";
    case "paused":
      return "offline";
    case "error":
      return "error";
    case "completed":
      return "online";
  }
}

function isActiveStatus(status: AgenticStatus): boolean {
  return status !== "completed" && status !== "error";
}

export function ProjectLoopsTab({ projectId, hostId }: ProjectLoopsTabProps) {
  const navigate = useNavigate();
  const [loops, setLoops] = useState<AgenticLoop[]>([]);
  const [loading, setLoading] = useState(true);

  const fetchLoops = useCallback(() => {
    void api.loops.list({ project_id: projectId }).then(
      (data) => {
        setLoops(data);
        setLoading(false);
      },
      () => setLoading(false),
    );
  }, [projectId]);

  useEffect(() => {
    fetchLoops();
  }, [fetchLoops]);

  // Listen for real-time loop updates
  useEffect(() => {
    const handler = () => fetchLoops();
    window.addEventListener("myremote:agentic-loop-update", handler);
    return () =>
      window.removeEventListener("myremote:agentic-loop-update", handler);
  }, [fetchLoops]);

  // Fallback polling every 15s
  useEffect(() => {
    const interval = setInterval(fetchLoops, 15_000);
    return () => clearInterval(interval);
  }, [fetchLoops]);

  const activeLoops = useMemo(
    () => loops.filter((l) => isActiveStatus(l.status)),
    [loops],
  );
  const historyLoops = useMemo(
    () => loops.filter((l) => !isActiveStatus(l.status)),
    [loops],
  );

  const handleLoopClick = useCallback(
    (loop: AgenticLoop) => {
      void navigate(
        `/hosts/${hostId}/sessions/${loop.session_id}/loops/${loop.id}`,
      );
    },
    [navigate, hostId],
  );

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12 text-sm text-text-tertiary">
        Loading loops...
      </div>
    );
  }

  if (loops.length === 0) {
    return (
      <div className="flex items-center justify-center py-12 text-sm text-text-tertiary">
        No agentic loops for this project yet.
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* Active section */}
      {activeLoops.length > 0 && (
        <div>
          <div className="mb-3 flex items-center justify-between">
            <h2 className="text-sm font-medium text-accent">
              Active ({activeLoops.length})
            </h2>
            <Button onClick={fetchLoops} variant="ghost" size="sm">
              <RefreshCw size={14} />
              Refresh
            </Button>
          </div>
          <div className="space-y-2">
            {activeLoops.map((loop) => (
              <LoopCard
                key={loop.id}
                loop={loop}
                onClick={() => handleLoopClick(loop)}
              />
            ))}
          </div>
        </div>
      )}

      {/* History section */}
      {historyLoops.length > 0 && (
        <div>
          <div className="mb-3 flex items-center justify-between">
            <h2 className="text-sm font-medium text-text-tertiary">
              History ({historyLoops.length})
            </h2>
            {activeLoops.length === 0 && (
              <Button onClick={fetchLoops} variant="ghost" size="sm">
                <RefreshCw size={14} />
                Refresh
              </Button>
            )}
          </div>
          <div className="space-y-2">
            {historyLoops.map((loop) => (
              <LoopCard
                key={loop.id}
                loop={loop}
                onClick={() => handleLoopClick(loop)}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function LoopCard({
  loop,
  onClick,
}: {
  loop: AgenticLoop;
  onClick: () => void;
}) {
  const isActive = isActiveStatus(loop.status);
  const dot = statusDotProps(loop.status);
  const badgeVariant = statusBadgeVariant(loop.status);

  const borderClass =
    loop.status === "working"
      ? "border-l-2 border-l-accent"
      : loop.status === "waiting_for_input"
        ? "border-l-2 border-l-status-warning animate-pulse"
        : isActive
          ? "border-l-2 border-l-status-offline"
          : "";

  return (
    <div
      onClick={onClick}
      className={`cursor-pointer rounded-md border border-border bg-bg-secondary px-4 py-3 transition-colors duration-150 hover:bg-bg-hover ${borderClass}`}
    >
      {/* Row 1: status dot, tool name, badge, cost & tokens */}
      <div className="flex items-center gap-2">
        <StatusDot status={dot.status} pulse={dot.pulse} />
        <span className="text-sm font-medium text-text-primary">
          {loop.tool_name}
        </span>
        <Badge variant={badgeVariant}>{loop.status}</Badge>
        {loop.status === "waiting_for_input" && loop.pending_tool_calls > 0 && (
          <span className="rounded bg-status-warning/15 px-1.5 py-0.5 text-[10px] font-medium text-status-warning">
            {loop.pending_tool_calls} pending
          </span>
        )}
        <div className="ml-auto flex items-center gap-3">
          <span className="text-xs text-text-secondary">
            {formatCost(loop.estimated_cost_usd)}
          </span>
          <span className="font-mono text-xs text-text-tertiary">
            {formatTokens(loop.total_tokens_in)} /{" "}
            {formatTokens(loop.total_tokens_out)}
          </span>
        </div>
      </div>

      {/* Row 2: project path, relative time, duration */}
      <div className="mt-1 flex items-center gap-1.5 text-xs text-text-tertiary">
        {loop.project_path && (
          <>
            <span className="max-w-[300px] truncate font-mono">
              {loop.project_path}
            </span>
            <span>·</span>
          </>
        )}
        <span>{formatRelativeTime(loop.started_at)}</span>
        {loop.ended_at && (
          <>
            <span>·</span>
            <span>{formatDuration(loop.started_at, loop.ended_at)}</span>
          </>
        )}
      </div>
    </div>
  );
}
