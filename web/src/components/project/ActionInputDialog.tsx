import { Loader2, Play, RefreshCw, X, AlertCircle } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import {
  api,
  type ProjectAction,
  type ResolvedActionInput,
  type RunActionRequest,
} from "../../lib/api";
import { Button } from "../ui/Button";
import { Input } from "../ui/Input";
import { IconButton } from "../ui/IconButton";
import { showToast } from "../layout/Toast";
import { detectMissingInputs, getActionIcon } from "./action-utils";

interface ActionInputDialogProps {
  action: ProjectAction;
  projectId: string;
  hostId: string;
  worktreePath?: string;
  worktreeBranch?: string;
  onClose: () => void;
}

export function ActionInputDialog({
  action,
  projectId,
  hostId,
  worktreePath,
  worktreeBranch,
  onClose,
}: ActionInputDialogProps) {
  const navigate = useNavigate();
  const firstInputRef = useRef<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement | null>(null);

  // Determine if worktree/branch inputs are needed from command template vars
  const { needsWorktree, needsBranch } = useMemo(
    () => detectMissingInputs(action.command, action.working_dir, worktreePath, worktreeBranch),
    [action.command, action.working_dir, worktreePath, worktreeBranch],
  );

  // Form state for custom inputs
  const [values, setValues] = useState<Record<string, string>>(() => {
    const defaults: Record<string, string> = {};
    for (const input of action.inputs ?? []) {
      if (input.default) {
        defaults[input.name] = input.default;
      }
    }
    return defaults;
  });

  // Worktree/branch form state
  const [worktreeValue, setWorktreeValue] = useState(worktreePath ?? "");
  const [branchValue, setBranchValue] = useState(worktreeBranch ?? "");
  const [worktreeOptions, setWorktreeOptions] = useState<{ path: string; label: string }[]>([]);
  const [worktreeLoading, setWorktreeLoading] = useState(needsWorktree);

  // Script resolution state
  const [resolvedInputs, setResolvedInputs] = useState<Record<string, ResolvedActionInput>>({});
  const [resolving, setResolving] = useState(false);

  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const hasScriptedInputs = useMemo(
    () => (action.inputs ?? []).some((i) => i.script),
    [action.inputs],
  );

  // Focus first input on mount
  useEffect(() => {
    const timer = setTimeout(() => firstInputRef.current?.focus(), 50);
    return () => clearTimeout(timer);
  }, []);

  // Fetch worktrees if needed
  useEffect(() => {
    if (!needsWorktree) return;
    let cancelled = false;
    void api.projects.worktrees(projectId).then((wts) => {
      if (cancelled) return;
      const opts = wts.map((wt) => ({
        path: wt.path,
        label: wt.git_branch ?? wt.name,
      }));
      setWorktreeOptions(opts);
      if (opts.length > 0 && !worktreeValue) {
        setWorktreeValue(opts[0]!.path);
        // Also set branch from the first worktree
        const firstWt = wts[0];
        if (firstWt?.git_branch) setBranchValue(firstWt.git_branch);
      }
      setWorktreeLoading(false);
    }).catch(() => {
      if (!cancelled) setWorktreeLoading(false);
    });
    return () => { cancelled = true; };
  }, [projectId, needsWorktree, worktreeValue]);

  // Resolve scripted inputs
  const resolveScripts = useCallback(async () => {
    if (!hasScriptedInputs) return;
    setResolving(true);
    try {
      const result = await api.projects.resolveActionInputs(projectId, action.name);
      const map: Record<string, ResolvedActionInput> = {};
      for (const ri of result.inputs) {
        map[ri.name] = ri;
      }
      setResolvedInputs(map);
    } catch (err) {
      // Mark all scripted inputs as errored
      const map: Record<string, ResolvedActionInput> = {};
      for (const input of action.inputs ?? []) {
        if (input.script) {
          map[input.name] = {
            name: input.name,
            options: [],
            error: err instanceof Error ? err.message : "Failed to resolve options",
          };
        }
      }
      setResolvedInputs(map);
    } finally {
      setResolving(false);
    }
  }, [hasScriptedInputs, projectId, action.name, action.inputs]);

  useEffect(() => {
    void resolveScripts();
  }, [resolveScripts]);

  const setValue = useCallback((name: string, value: string) => {
    setValues((prev) => ({ ...prev, [name]: value }));
  }, []);

  const handleSubmit = useCallback(async () => {
    if (loading) return;

    // Validate required fields
    for (const input of action.inputs ?? []) {
      const isRequired = input.required !== false;
      if (isRequired && !values[input.name]?.trim()) {
        setError(`Field "${input.label ?? input.name}" is required`);
        return;
      }
    }

    // Validate worktree/branch if needed
    if (needsWorktree && !worktreeValue) {
      setError("Worktree is required");
      return;
    }
    if (needsBranch && !branchValue.trim()) {
      setError("Branch is required");
      return;
    }

    setError(null);
    setLoading(true);

    try {
      const body: RunActionRequest = {
        inputs: values,
      };
      if (worktreePath || worktreeValue) body.worktree_path = worktreeValue || worktreePath;
      if (worktreeBranch || branchValue) body.branch = branchValue || worktreeBranch;

      const result = await api.projects.runAction(projectId, action.name, body);
      void navigate(`/hosts/${hostId}/sessions/${result.session_id}`);
      onClose();
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Failed to run action";
      setError(msg);
      showToast(msg, "error");
    } finally {
      setLoading(false);
    }
  }, [
    loading, action, values, needsWorktree, needsBranch, worktreeValue,
    branchValue, worktreePath, worktreeBranch, projectId, hostId, navigate, onClose,
  ]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        void handleSubmit();
      }
    },
    [onClose, handleSubmit],
  );

  // Live command preview - substitute current values into command
  const commandPreview = useMemo(() => {
    let cmd = action.command;
    for (const [key, val] of Object.entries(values)) {
      cmd = cmd.replaceAll(`{{${key}}}`, val || `{{${key}}}`);
    }
    if (worktreeValue) {
      cmd = cmd.replaceAll("{{worktree_path}}", worktreeValue);
      // Extract worktree name from path
      const wtName = worktreeValue.split("/").pop() ?? worktreeValue;
      cmd = cmd.replaceAll("{{worktree_name}}", wtName);
    }
    if (branchValue) {
      cmd = cmd.replaceAll("{{branch}}", branchValue);
    }
    return cmd;
  }, [action.command, values, worktreeValue, branchValue]);

  const Icon = getActionIcon(action.icon);
  const inputs = action.inputs ?? [];
  let inputIndex = 0;

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
              {action.name}
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

        {action.description && (
          <p className="mb-4 text-xs text-text-tertiary">
            {action.description}
          </p>
        )}

        <div className="flex flex-col gap-4">
          {/* Worktree selector (if needed) */}
          {needsWorktree && (
            <div className="flex flex-col gap-1.5">
              <label htmlFor="action-worktree-select" className="text-xs font-medium text-text-secondary">
                Worktree *
              </label>
              {worktreeLoading ? (
                <div className="h-8 animate-pulse rounded-md bg-bg-tertiary" data-testid="worktree-skeleton" />
              ) : worktreeOptions.length === 0 ? (
                <div className="flex items-center gap-2 rounded-md border border-border bg-bg-tertiary px-3 py-2">
                  <AlertCircle size={14} className="text-text-tertiary" />
                  <span className="text-xs text-text-secondary">No worktrees found</span>
                </div>
              ) : (
                <select
                  id="action-worktree-select"
                  value={worktreeValue}
                  onChange={(e) => {
                    setWorktreeValue(e.target.value);
                    const wt = worktreeOptions.find((w) => w.path === e.target.value);
                    if (wt) setBranchValue(wt.label);
                  }}
                  className="h-8 rounded-md border border-border bg-bg-tertiary px-3 text-sm text-text-primary transition-colors duration-150 focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                >
                  {worktreeOptions.map((wt) => (
                    <option key={wt.path} value={wt.path}>
                      {wt.label}
                    </option>
                  ))}
                </select>
              )}
            </div>
          )}

          {/* Branch input (if needed and not covered by worktree selector) */}
          {needsBranch && !needsWorktree && (
            <Input
              id="action-branch-input"
              label="Branch *"
              placeholder="e.g. feature/my-branch"
              value={branchValue}
              onChange={(e) => setBranchValue(e.target.value)}
            />
          )}

          {/* Dynamic form inputs */}
          {inputs.map((input) => {
            const currentIdx = inputIndex++;
            const isRequired = input.required !== false;
            const label = `${input.label ?? input.name}${isRequired ? " *" : ""}`;
            const resolved = resolvedInputs[input.name];
            const isScripted = !!input.script;
            const isScriptLoading = isScripted && resolving && !resolved;

            if (input.input_type === "multiline") {
              return (
                <div key={input.name} className="flex flex-col gap-1.5">
                  <label htmlFor={`action-input-${input.name}`} className="text-xs font-medium text-text-secondary">
                    {label}
                  </label>
                  <textarea
                    id={`action-input-${input.name}`}
                    ref={currentIdx === 0 && !needsWorktree && !needsBranch ? (el) => { firstInputRef.current = el; } : undefined}
                    value={values[input.name] ?? ""}
                    onChange={(e) => setValue(input.name, e.target.value)}
                    placeholder={input.placeholder}
                    rows={4}
                    className="rounded-md border border-border bg-bg-tertiary px-3 py-2 text-sm text-text-primary transition-colors duration-150 placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                  />
                </div>
              );
            }

            if (input.input_type === "select") {
              return (
                <div key={input.name} className="flex flex-col gap-1.5">
                  <label htmlFor={`action-input-${input.name}`} className="text-xs font-medium text-text-secondary">
                    {label}
                  </label>
                  {isScriptLoading ? (
                    <div className="h-8 animate-pulse rounded-md bg-bg-tertiary" data-testid={`skeleton-${input.name}`} />
                  ) : resolved?.error ? (
                    <div className="flex flex-col gap-1">
                      <div className="flex h-8 items-center justify-between rounded-md border border-status-error/30 bg-status-error/10 px-3">
                        <span className="text-xs text-status-error">Failed to load options</span>
                        <IconButton
                          icon={RefreshCw}
                          aria-label={`Retry loading ${input.label ?? input.name}`}
                          onClick={() => void resolveScripts()}
                          className="h-6 w-6"
                        />
                      </div>
                      <span className="text-xs text-status-error" data-testid={`error-${input.name}`}>{resolved.error}</span>
                    </div>
                  ) : isScripted && resolved && resolved.options.length === 0 ? (
                    <div className="flex h-8 items-center gap-2 rounded-md border border-border bg-bg-tertiary px-3">
                      <AlertCircle size={14} className="text-text-tertiary" />
                      <span className="text-xs text-text-secondary">No options available</span>
                      <IconButton
                        icon={RefreshCw}
                        aria-label={`Retry loading ${input.label ?? input.name}`}
                        onClick={() => void resolveScripts()}
                        className="ml-auto h-6 w-6"
                      />
                    </div>
                  ) : (
                    <select
                      id={`action-input-${input.name}`}
                      ref={currentIdx === 0 && !needsWorktree && !needsBranch ? (el) => { firstInputRef.current = el as unknown as HTMLInputElement; } : undefined}
                      value={values[input.name] ?? ""}
                      onChange={(e) => setValue(input.name, e.target.value)}
                      className="h-8 rounded-md border border-border bg-bg-tertiary px-3 text-sm text-text-primary transition-colors duration-150 focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                    >
                      <option value="">{input.placeholder ?? "Select..."}</option>
                      {isScripted && resolved
                        ? resolved.options.map((opt) => (
                            <option key={opt.value} value={opt.value}>
                              {opt.label ?? opt.value}
                            </option>
                          ))
                        : (input.options ?? []).map((opt) => (
                            <option key={opt} value={opt}>
                              {opt}
                            </option>
                          ))}
                    </select>
                  )}
                </div>
              );
            }

            // Default: text input
            return (
              <Input
                key={input.name}
                id={`action-input-${input.name}`}
                ref={currentIdx === 0 && !needsWorktree && !needsBranch ? (el) => { firstInputRef.current = el; } : undefined}
                label={label}
                value={values[input.name] ?? ""}
                onChange={(e) => setValue(input.name, e.target.value)}
                placeholder={input.placeholder}
              />
            );
          })}

          {/* Live command preview */}
          <div className="flex flex-col gap-1.5">
            <span className="text-xs font-medium text-text-secondary">Command preview</span>
            <pre className="whitespace-pre-wrap break-all rounded-md bg-bg-tertiary p-3 font-mono text-xs text-text-tertiary">
              {commandPreview}
            </pre>
          </div>

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
              <>
                <Play size={14} />
                Run action
              </>
            )}
          </Button>
        </div>
      </div>
    </div>
  );
}
