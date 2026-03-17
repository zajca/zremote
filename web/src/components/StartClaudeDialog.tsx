import { Bot, ChevronDown, History, Loader2, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { api } from "../lib/api";
import type { ClaudeTask, ToolPreset } from "../types/claude-session";
import { Button } from "./ui/Button";
import { Input } from "./ui/Input";

interface StartClaudeDialogProps {
  projectName: string;
  projectPath: string;
  hostId: string;
  projectId?: string;
  initialPrompt?: string;
  onClose: () => void;
}

const MODEL_OPTIONS = [
  { value: "sonnet", label: "Sonnet" },
  { value: "opus", label: "Opus" },
  { value: "haiku", label: "Haiku" },
] as const;

const TOOL_PRESET_OPTIONS: { value: ToolPreset; label: string }[] = [
  { value: "standard", label: "Standard" },
  { value: "read_only", label: "Read only" },
  { value: "full_access", label: "Full access" },
  { value: "custom", label: "Custom" },
];

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

export function StartClaudeDialog({
  projectName,
  projectPath,
  hostId,
  projectId,
  initialPrompt,
  onClose,
}: StartClaudeDialogProps) {
  const navigate = useNavigate();
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const [prompt, setPrompt] = useState(initialPrompt ?? "");
  const [model, setModel] = useState("sonnet");
  const [optionsOpen, setOptionsOpen] = useState(false);
  const [toolPreset, setToolPreset] = useState<ToolPreset>("standard");
  const [customTools, setCustomTools] = useState("");
  const [skipPermissions, setSkipPermissions] = useState(false);
  const [customFlags, setCustomFlags] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Resume-related state
  const [completedTasks, setCompletedTasks] = useState<ClaudeTask[]>([]);
  const [loadingPrevious, setLoadingPrevious] = useState(false);
  const [resumingId, setResumingId] = useState<string | null>(null);

  useEffect(() => {
    if (initialPrompt) {
      setPrompt(initialPrompt);
    }
  }, [initialPrompt]);

  useEffect(() => {
    const timer = setTimeout(() => textareaRef.current?.focus(), 50);
    return () => clearTimeout(timer);
  }, []);

  // Fetch completed tasks for resume
  useEffect(() => {
    setLoadingPrevious(true);
    const filters: { host_id: string; status: string; project_id?: string } = {
      host_id: hostId,
      status: "completed",
    };
    if (projectId) {
      filters.project_id = projectId;
    }
    void api.claudeTasks.list(filters).then(
      (tasks) => {
        setCompletedTasks(tasks.slice(0, 5));
        setLoadingPrevious(false);
      },
      () => setLoadingPrevious(false),
    );
  }, [hostId, projectId]);

  const handleSubmit = useCallback(async () => {
    if (loading) return;
    setError(null);
    setLoading(true);

    try {
      const allowedTools =
        toolPreset === "custom" && customTools.trim()
          ? customTools
              .split(",")
              .map((t) => t.trim())
              .filter(Boolean)
          : undefined;

      const task = await api.claudeTasks.create({
        host_id: hostId,
        project_path: projectPath,
        project_id: projectId,
        model,
        initial_prompt: prompt.trim() || undefined,
        allowed_tools: allowedTools,
        skip_permissions: skipPermissions || undefined,
        custom_flags: customFlags.trim() || undefined,
      });

      void navigate(`/hosts/${hostId}/sessions/${task.session_id}`);
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to start Claude task");
    } finally {
      setLoading(false);
    }
  }, [
    loading,
    hostId,
    projectPath,
    projectId,
    model,
    prompt,
    toolPreset,
    customTools,
    skipPermissions,
    customFlags,
    navigate,
    onClose,
  ]);

  const handleResume = useCallback(
    async (taskId: string) => {
      setResumingId(taskId);
      setError(null);
      try {
        const newTask = await api.claudeTasks.resume(taskId);
        void navigate(`/hosts/${hostId}/sessions/${newTask.session_id}`);
        onClose();
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to resume task");
      } finally {
        setResumingId(null);
      }
    },
    [navigate, hostId, onClose],
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      } else if (e.key === "Enter" && e.metaKey) {
        e.preventDefault();
        void handleSubmit();
      }
    },
    [onClose, handleSubmit],
  );

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
      onKeyDown={handleKeyDown}
    >
      <div
        className="max-h-[90vh] w-full max-w-lg overflow-y-auto rounded-lg border border-border bg-bg-primary p-6 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Bot size={18} className="text-accent" />
            <h2 className="text-lg font-semibold text-text-primary">
              Start Claude
            </h2>
          </div>
          <button
            onClick={onClose}
            className="rounded p-1 text-text-tertiary transition-colors hover:bg-bg-hover hover:text-text-primary"
          >
            <X size={16} />
          </button>
        </div>

        <p className="mb-4 text-xs text-text-tertiary">
          Project: {projectName}
        </p>

        {/* Resume previous section */}
        {!loadingPrevious && completedTasks.length > 0 && (
          <div className="mb-4">
            <div className="mb-2 flex items-center gap-1.5 text-xs font-medium text-text-secondary">
              <History size={12} />
              Resume previous
            </div>
            <div className="space-y-1.5">
              {completedTasks.map((task) => (
                <button
                  key={task.id}
                  onClick={() => void handleResume(task.id)}
                  disabled={resumingId !== null}
                  className="flex w-full items-center gap-2 rounded-md border border-border bg-bg-secondary px-3 py-2 text-left transition-colors duration-150 hover:bg-bg-hover disabled:opacity-50"
                >
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-xs font-medium text-text-primary">
                      {task.initial_prompt ?? task.model ?? "Claude task"}
                    </p>
                    <p className="text-[10px] text-text-tertiary">
                      {task.model && `${task.model} · `}
                      {formatRelativeTime(task.started_at)}
                      {task.total_cost_usd > 0 && ` · $${task.total_cost_usd.toFixed(2)}`}
                    </p>
                    {task.summary && (
                      <p className="mt-0.5 line-clamp-1 text-[10px] text-text-tertiary">
                        {task.summary}
                      </p>
                    )}
                  </div>
                  {resumingId === task.id ? (
                    <Loader2 size={12} className="shrink-0 animate-spin text-accent" />
                  ) : (
                    <span className="shrink-0 text-[10px] text-accent">Resume</span>
                  )}
                </button>
              ))}
            </div>
            <div className="my-4 border-t border-border" />
          </div>
        )}

        <div className="flex flex-col gap-4">
          {/* Prompt textarea */}
          <div className="flex flex-col gap-1.5">
            <label className="text-xs font-medium text-text-secondary">
              Prompt
            </label>
            <textarea
              ref={textareaRef}
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder="What should Claude do?"
              rows={4}
              className="rounded-md border border-border bg-bg-tertiary px-3 py-2 text-sm text-text-primary transition-colors duration-150 placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
            />
          </div>

          {/* Model segmented control */}
          <div className="flex flex-col gap-1.5">
            <label className="text-xs font-medium text-text-secondary">
              Model
            </label>
            <div className="flex rounded-md border border-border">
              {MODEL_OPTIONS.map((opt) => (
                <button
                  key={opt.value}
                  onClick={() => setModel(opt.value)}
                  className={`flex-1 px-3 py-1.5 text-xs font-medium transition-colors duration-150 first:rounded-l-md last:rounded-r-md ${
                    model === opt.value
                      ? "bg-accent text-white"
                      : "bg-bg-tertiary text-text-secondary hover:bg-bg-hover hover:text-text-primary"
                  }`}
                >
                  {opt.label}
                </button>
              ))}
            </div>
          </div>

          {/* Collapsible options */}
          <button
            onClick={() => setOptionsOpen((prev) => !prev)}
            className="flex items-center gap-1.5 text-xs font-medium text-text-secondary transition-colors hover:text-text-primary"
          >
            <ChevronDown
              size={12}
              className={`transition-transform duration-150 ${optionsOpen ? "" : "-rotate-90"}`}
            />
            Options
          </button>

          {optionsOpen && (
            <div className="flex flex-col gap-4 rounded-md border border-border bg-bg-secondary p-4">
              {/* Tool preset */}
              <div className="flex flex-col gap-1.5">
                <label className="text-xs font-medium text-text-secondary">
                  Tool preset
                </label>
                <select
                  value={toolPreset}
                  onChange={(e) => setToolPreset(e.target.value as ToolPreset)}
                  className="h-8 rounded-md border border-border bg-bg-tertiary px-3 text-sm text-text-primary transition-colors duration-150 focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                >
                  {TOOL_PRESET_OPTIONS.map((opt) => (
                    <option key={opt.value} value={opt.value}>
                      {opt.label}
                    </option>
                  ))}
                </select>
              </div>

              {/* Custom tools */}
              {toolPreset === "custom" && (
                <Input
                  label="Allowed tools (comma-separated)"
                  placeholder="Read, Edit, Bash, Grep"
                  value={customTools}
                  onChange={(e) => setCustomTools(e.target.value)}
                />
              )}

              {/* Skip permissions */}
              <label className="flex items-center gap-2">
                <input
                  type="checkbox"
                  checked={skipPermissions}
                  onChange={(e) => setSkipPermissions(e.target.checked)}
                  className="h-4 w-4 rounded border-border accent-accent"
                />
                <span className="text-xs text-text-secondary">
                  Skip permissions
                </span>
                {skipPermissions && (
                  <span className="text-xs text-status-warning">
                    Tools will run without approval
                  </span>
                )}
              </label>

              {/* Custom flags */}
              <Input
                label="Custom flags"
                placeholder="--verbose --max-turns 50"
                value={customFlags}
                onChange={(e) => setCustomFlags(e.target.value)}
              />
            </div>
          )}

          {/* Error display */}
          {error && (
            <div className="rounded-md border border-status-error/30 bg-status-error/10 px-3 py-2 text-xs text-status-error">
              {error}
            </div>
          )}
        </div>

        <div className="mt-6 flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onClose} disabled={loading}>
            Cancel
          </Button>
          <Button size="sm" onClick={() => void handleSubmit()} disabled={loading}>
            {loading ? (
              <>
                <Loader2 size={14} className="animate-spin" />
                Starting...
              </>
            ) : (
              "Start Claude"
            )}
          </Button>
        </div>
      </div>
    </div>
  );
}
