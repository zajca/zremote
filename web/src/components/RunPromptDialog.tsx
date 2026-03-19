import {
  Code,
  Bug,
  FileText,
  Zap,
  Bot,
  Search,
  Settings,
  Terminal,
  Play,
  Loader2,
  X,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { api } from "../lib/api";
import type { PromptTemplate, PromptExecMode } from "../types/prompt";
import { usePendingPasteStore } from "../stores/pending-paste-store";
import { Button } from "./ui/Button";
import { Input } from "./ui/Input";
import { showToast } from "./layout/Toast";

const ICON_MAP: Record<string, LucideIcon> = {
  code: Code,
  bug: Bug,
  "file-text": FileText,
  zap: Zap,
  bot: Bot,
  search: Search,
  settings: Settings,
  terminal: Terminal,
  play: Play,
};

function getIcon(name?: string): LucideIcon {
  return (name && ICON_MAP[name]) || FileText;
}

const MODEL_OPTIONS = [
  { value: "sonnet", label: "Sonnet" },
  { value: "opus", label: "Opus" },
  { value: "haiku", label: "Haiku" },
] as const;

interface RunPromptDialogProps {
  template: PromptTemplate;
  projectId: string;
  projectPath: string;
  hostId: string;
  projectName: string;
  worktreePath?: string;
  branch?: string;
  currentSessionId?: string;
  onClose: () => void;
}

export function RunPromptDialog({
  template,
  projectId,
  projectPath,
  hostId,
  projectName,
  worktreePath,
  branch,
  currentSessionId,
  onClose,
}: RunPromptDialogProps) {
  const navigate = useNavigate();
  const firstInputRef = useRef<HTMLInputElement | HTMLTextAreaElement | null>(null);

  const [inputs, setInputs] = useState<Record<string, string>>(() => {
    const defaults: Record<string, string> = {};
    for (const input of template.inputs) {
      if (input.default) {
        defaults[input.name] = input.default;
      }
    }
    return defaults;
  });

  const [mode, setMode] = useState<PromptExecMode>(
    template.default_mode ?? "paste_to_terminal",
  );
  const [model, setModel] = useState(template.model ?? "sonnet");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const timer = setTimeout(() => firstInputRef.current?.focus(), 50);
    return () => clearTimeout(timer);
  }, []);

  const setInput = useCallback((name: string, value: string) => {
    setInputs((prev) => ({ ...prev, [name]: value }));
  }, []);

  const handleSubmit = useCallback(async () => {
    if (loading) return;

    // Validate required fields
    for (const input of template.inputs) {
      const isRequired = input.required !== false;
      if (isRequired && !inputs[input.name]?.trim()) {
        setError(`Field "${input.label ?? input.name}" is required`);
        return;
      }
    }

    setError(null);
    setLoading(true);

    try {
      const resolved = await api.projects.resolvePrompt(projectId, template.name, {
        inputs,
        worktree_path: worktreePath,
        branch,
      });

      if (mode === "claude_session") {
        const task = await api.claudeTasks.create({
          host_id: hostId,
          project_path: projectPath,
          project_id: projectId,
          model,
          initial_prompt: resolved.prompt,
          allowed_tools: template.allowed_tools,
          skip_permissions: template.skip_permissions,
        });
        void navigate(`/hosts/${hostId}/sessions/${task.session_id}`);
        onClose();
      } else {
        // paste_to_terminal mode
        if (currentSessionId) {
          usePendingPasteStore.getState().setPendingPaste(currentSessionId, resolved.prompt);
          onClose();
        } else {
          // Create a new session, then paste
          const session = await api.sessions.create(hostId, {
            workingDir: worktreePath ?? projectPath,
          });
          usePendingPasteStore.getState().setPendingPaste(session.id, resolved.prompt);
          void navigate(`/hosts/${hostId}/sessions/${session.id}`);
          onClose();
        }
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Failed to run prompt";
      setError(msg);
      showToast(msg, "error");
    } finally {
      setLoading(false);
    }
  }, [
    loading,
    template,
    inputs,
    projectId,
    worktreePath,
    branch,
    mode,
    hostId,
    projectPath,
    model,
    currentSessionId,
    navigate,
    onClose,
  ]);

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

  const Icon = getIcon(template.icon);

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
        {/* Header */}
        <div className="mb-4 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Icon size={18} className="text-accent" />
            <h2 className="text-lg font-semibold text-text-primary">
              {template.name}
            </h2>
          </div>
          <button
            onClick={onClose}
            className="rounded p-1 text-text-tertiary transition-colors hover:bg-bg-hover hover:text-text-primary"
            aria-label="Close"
          >
            <X size={16} />
          </button>
        </div>

        {template.description && (
          <p className="mb-4 text-xs text-text-tertiary">
            {template.description}
          </p>
        )}

        <p className="mb-4 text-xs text-text-tertiary">
          Project: {projectName}
        </p>

        <div className="flex flex-col gap-4">
          {/* Dynamic form inputs */}
          {template.inputs.map((input, idx) => {
            const isRequired = input.required !== false;
            const label = `${input.label ?? input.name}${isRequired ? " *" : ""}`;

            if (input.input_type === "multiline") {
              return (
                <div key={input.name} className="flex flex-col gap-1.5">
                  <label className="text-xs font-medium text-text-secondary">
                    {label}
                  </label>
                  <textarea
                    ref={idx === 0 ? (el) => { firstInputRef.current = el; } : undefined}
                    value={inputs[input.name] ?? ""}
                    onChange={(e) => setInput(input.name, e.target.value)}
                    placeholder={input.placeholder}
                    rows={4}
                    className="rounded-md border border-border bg-bg-tertiary px-3 py-2 text-sm text-text-primary transition-colors duration-150 placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                  />
                </div>
              );
            }

            if (input.input_type === "select" && input.options) {
              return (
                <div key={input.name} className="flex flex-col gap-1.5">
                  <label className="text-xs font-medium text-text-secondary">
                    {label}
                  </label>
                  <select
                    ref={idx === 0 ? (el) => { firstInputRef.current = el as unknown as HTMLInputElement; } : undefined}
                    value={inputs[input.name] ?? ""}
                    onChange={(e) => setInput(input.name, e.target.value)}
                    className="h-8 rounded-md border border-border bg-bg-tertiary px-3 text-sm text-text-primary transition-colors duration-150 focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                  >
                    <option value="">{input.placeholder ?? "Select..."}</option>
                    {input.options.map((opt) => (
                      <option key={opt} value={opt}>
                        {opt}
                      </option>
                    ))}
                  </select>
                </div>
              );
            }

            // Default: text input
            return (
              <Input
                key={input.name}
                ref={idx === 0 ? (el) => { firstInputRef.current = el; } : undefined}
                label={label}
                value={inputs[input.name] ?? ""}
                onChange={(e) => setInput(input.name, e.target.value)}
                placeholder={input.placeholder}
              />
            );
          })}

          {/* Execution mode toggle */}
          <div className="flex flex-col gap-1.5">
            <label className="text-xs font-medium text-text-secondary">
              Execution mode
            </label>
            <div className="flex rounded-md border border-border">
              <button
                onClick={() => setMode("paste_to_terminal")}
                className={`flex-1 px-3 py-1.5 text-xs font-medium transition-colors duration-150 first:rounded-l-md last:rounded-r-md ${
                  mode === "paste_to_terminal"
                    ? "bg-accent text-white"
                    : "bg-bg-tertiary text-text-secondary hover:bg-bg-hover hover:text-text-primary"
                }`}
              >
                Paste to terminal
              </button>
              <button
                onClick={() => setMode("claude_session")}
                className={`flex-1 px-3 py-1.5 text-xs font-medium transition-colors duration-150 first:rounded-l-md last:rounded-r-md ${
                  mode === "claude_session"
                    ? "bg-accent text-white"
                    : "bg-bg-tertiary text-text-secondary hover:bg-bg-hover hover:text-text-primary"
                }`}
              >
                Start Claude session
              </button>
            </div>
          </div>

          {/* Model selector for claude_session mode */}
          {mode === "claude_session" && (
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
                Running...
              </>
            ) : (
              "Run prompt"
            )}
          </Button>
        </div>
      </div>
    </div>
  );
}
