import { Bot, Play, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { api } from "../../lib/api";
import type { AgenticLoop, AgenticStatus } from "../../types/agentic";
import type { ClaudeTask, ClaudeTaskStatus } from "../../types/claude-session";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import { StatusDot } from "../ui/StatusDot";
import { showToast } from "../layout/Toast";

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

function taskStatusBadgeVariant(
  status: ClaudeTaskStatus,
): "online" | "offline" | "error" | "warning" | "creating" {
  switch (status) {
    case "starting":
      return "creating";
    case "active":
      return "creating";
    case "completed":
      return "online";
    case "error":
      return "error";
  }
}

function isActiveStatus(status: AgenticStatus): boolean {
  return status !== "completed" && status !== "error";
}

function isActiveTaskStatus(status: ClaudeTaskStatus): boolean {
  return status === "starting" || status === "active";
}

export function ProjectLoopsTab({ projectId, hostId }: ProjectLoopsTabProps) {
  const navigate = useNavigate();
  const [loops, setLoops] = useState<AgenticLoop[]>([]);
  const [tasks, setTasks] = useState<ClaudeTask[]>([]);
  const [loading, setLoading] = useState(true);
  const [resumingId, setResumingId] = useState<string | null>(null);

  const fetchLoops = useCallback(() => {
    void api.loops.list({ project_id: projectId }).then(
      (data) => {
        setLoops(data);
        setLoading(false);
      },
      () => setLoading(false),
    );
  }, [projectId]);

  const fetchTasks = useCallback(() => {
    void api.claudeTasks.list({ project_id: projectId }).then(
      (data) => setTasks(data),
      () => {},
    );
  }, [projectId]);

  useEffect(() => {
    fetchLoops();
    fetchTasks();
  }, [fetchLoops, fetchTasks]);

  // Listen for real-time loop updates
  useEffect(() => {
    const handler = () => fetchLoops();
    window.addEventListener("zremote:agentic-loop-update", handler);
    return () =>
      window.removeEventListener("zremote:agentic-loop-update", handler);
  }, [fetchLoops]);

  // Listen for real-time task updates
  useEffect(() => {
    const handler = () => fetchTasks();
    window.addEventListener("zremote:claude-task-update", handler);
    return () =>
      window.removeEventListener("zremote:claude-task-update", handler);
  }, [fetchTasks]);

  // Fallback polling every 15s
  useEffect(() => {
    const interval = setInterval(() => {
      fetchLoops();
      fetchTasks();
    }, 15_000);
    return () => clearInterval(interval);
  }, [fetchLoops, fetchTasks]);

  const activeLoops = useMemo(
    () => loops.filter((l) => isActiveStatus(l.status)),
    [loops],
  );
  const historyLoops = useMemo(
    () => loops.filter((l) => !isActiveStatus(l.status)),
    [loops],
  );

  const activeTasks = useMemo(
    () => tasks.filter((t) => isActiveTaskStatus(t.status)),
    [tasks],
  );
  const completedTasks = useMemo(
    () => tasks.filter((t) => !isActiveTaskStatus(t.status)),
    [tasks],
  );

  const handleLoopClick = useCallback(
    (loop: AgenticLoop) => {
      void navigate(
        `/hosts/${hostId}/sessions/${loop.session_id}/loops/${loop.id}`,
      );
    },
    [navigate, hostId],
  );

  const handleTaskClick = useCallback(
    (task: ClaudeTask) => {
      void navigate(`/hosts/${hostId}/sessions/${task.session_id}`);
    },
    [navigate, hostId],
  );

  const handleResume = useCallback(
    async (taskId: string) => {
      setResumingId(taskId);
      try {
        const newTask = await api.claudeTasks.resume(taskId);
        void navigate(`/hosts/${hostId}/sessions/${newTask.session_id}`);
      } catch (err) {
        console.error("Failed to resume task:", err);
        showToast("Failed to resume task", "error");
      } finally {
        setResumingId(null);
      }
    },
    [navigate, hostId],
  );

  const handleRefresh = useCallback(() => {
    fetchLoops();
    fetchTasks();
  }, [fetchLoops, fetchTasks]);

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12 text-sm text-text-tertiary">
        Loading...
      </div>
    );
  }

  const hasContent = loops.length > 0 || tasks.length > 0;

  if (!hasContent) {
    return (
      <div className="flex items-center justify-center py-12 text-sm text-text-tertiary">
        No Claude tasks or agentic loops for this project yet.
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* Active Claude Tasks */}
      {activeTasks.length > 0 && (
        <div>
          <div className="mb-3 flex items-center justify-between">
            <h2 className="flex items-center gap-1.5 text-sm font-medium text-accent">
              <Bot size={14} />
              Active Tasks ({activeTasks.length})
            </h2>
            <Button onClick={handleRefresh} variant="ghost" size="sm">
              <RefreshCw size={14} />
              Refresh
            </Button>
          </div>
          <div className="space-y-2">
            {activeTasks.map((task) => (
              <TaskCard
                key={task.id}
                task={task}
                onClick={() => handleTaskClick(task)}
              />
            ))}
          </div>
        </div>
      )}

      {/* Active Agentic Loops */}
      {activeLoops.length > 0 && (
        <div>
          <div className="mb-3 flex items-center justify-between">
            <h2 className="text-sm font-medium text-accent">
              Active Loops ({activeLoops.length})
            </h2>
            {activeTasks.length === 0 && (
              <Button onClick={handleRefresh} variant="ghost" size="sm">
                <RefreshCw size={14} />
                Refresh
              </Button>
            )}
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

      {/* Completed Claude Tasks */}
      {completedTasks.length > 0 && (
        <div>
          <div className="mb-3 flex items-center justify-between">
            <h2 className="flex items-center gap-1.5 text-sm font-medium text-text-tertiary">
              <Bot size={14} />
              Completed Tasks ({completedTasks.length})
            </h2>
          </div>
          <div className="space-y-2">
            {completedTasks.map((task) => (
              <TaskCard
                key={task.id}
                task={task}
                onClick={() => handleTaskClick(task)}
                onResume={() => void handleResume(task.id)}
                resuming={resumingId === task.id}
              />
            ))}
          </div>
        </div>
      )}

      {/* History Agentic Loops */}
      {historyLoops.length > 0 && (
        <div>
          <div className="mb-3 flex items-center justify-between">
            <h2 className="text-sm font-medium text-text-tertiary">
              Loop History ({historyLoops.length})
            </h2>
            {activeTasks.length === 0 && activeLoops.length === 0 && (
              <Button onClick={handleRefresh} variant="ghost" size="sm">
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

function TaskCard({
  task,
  onClick,
  onResume,
  resuming,
}: {
  task: ClaudeTask;
  onClick: () => void;
  onResume?: () => void;
  resuming?: boolean;
}) {
  const isActive = isActiveTaskStatus(task.status);
  const badgeVariant = taskStatusBadgeVariant(task.status);

  const borderClass = isActive
    ? "border-l-2 border-l-accent"
    : "";

  return (
    <div
      onClick={onClick}
      className={`cursor-pointer rounded-md border border-border bg-bg-secondary px-4 py-3 transition-colors duration-150 hover:bg-bg-hover ${borderClass}`}
    >
      {/* Row 1: icon, model/prompt, badge, cost */}
      <div className="flex items-center gap-2">
        <Bot size={14} className="shrink-0 text-accent" />
        <span className="truncate text-sm font-medium text-text-primary">
          {task.initial_prompt
            ? task.initial_prompt.slice(0, 80) + (task.initial_prompt.length > 80 ? "..." : "")
            : task.model ?? "Claude task"}
        </span>
        <Badge variant={badgeVariant}>{task.status}</Badge>
        <div className="ml-auto flex items-center gap-3">
          {task.total_cost_usd > 0 && (
            <span className="text-xs text-text-secondary">
              {formatCost(task.total_cost_usd)}
            </span>
          )}
          {(task.total_tokens_in > 0 || task.total_tokens_out > 0) && (
            <span className="font-mono text-xs text-text-tertiary">
              {formatTokens(task.total_tokens_in)} / {formatTokens(task.total_tokens_out)}
            </span>
          )}
          {!isActive && onResume && (
            <Button
              variant="ghost"
              size="sm"
              onClick={(e) => {
                e.stopPropagation();
                onResume();
              }}
              disabled={resuming}
            >
              <Play size={12} />
              {resuming ? "Resuming..." : "Resume"}
            </Button>
          )}
        </div>
      </div>

      {/* Row 2: model, time, duration, summary */}
      <div className="mt-1 flex items-center gap-1.5 text-xs text-text-tertiary">
        {task.model && <span>{task.model}</span>}
        {task.model && <span>·</span>}
        <span>{formatRelativeTime(task.started_at)}</span>
        {task.ended_at && (
          <>
            <span>·</span>
            <span>{formatDuration(task.started_at, task.ended_at)}</span>
          </>
        )}
      </div>

      {/* Row 3: summary */}
      {task.summary && (
        <p className="mt-1.5 line-clamp-2 text-xs text-text-secondary">
          {task.summary}
        </p>
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
