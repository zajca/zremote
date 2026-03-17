import { AlertCircle, Bot, Check, ChevronDown, ChevronRight, Loader2, Plus, Trash2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import {
  api,
  type AgenticSettings,
  type ProjectAction,
  type ProjectSettings,
  type WorktreeSettings,
} from "../lib/api";
import type { LinearAction } from "../types/linear";
import { Button } from "./ui/Button";
import { Input } from "./ui/Input";
import { showToast } from "./layout/Toast";

const DEFAULT_LINEAR_ACTIONS: LinearAction[] = [
  {
    name: "Analyze",
    icon: "search",
    prompt: "Analyze issue {{issue.identifier}}: {{issue.title}}\n\n{{issue.description}}\n\nProvide a detailed analysis.",
  },
  {
    name: "Write RFC",
    icon: "file-text",
    prompt: "Write an RFC for issue {{issue.identifier}}: {{issue.title}}\n\n{{issue.description}}",
  },
  {
    name: "Implement",
    icon: "code",
    prompt: "Implement issue {{issue.identifier}}: {{issue.title}}\n\n{{issue.description}}",
  },
];

interface ProjectSettingsTabProps {
  projectId: string;
  projectPath: string;
  hostId: string;
}

interface EnvVar {
  key: string;
  value: string;
}

const ENV_NAME_REGEX = /^[A-Za-z_][A-Za-z0-9_]*$/;

function emptyAction(): ProjectAction {
  return {
    name: "",
    command: "",
    description: undefined,
    icon: undefined,
    working_dir: undefined,
    env: {},
    worktree_scoped: false,
  };
}

function defaultSettings(): ProjectSettings {
  return {
    env: {},
    agentic: {
      auto_detect: true,
      default_permissions: [],
      auto_approve_patterns: [],
    },
    actions: [],
  };
}

function settingsToEnvVars(settings: ProjectSettings): EnvVar[] {
  return Object.entries(settings.env).map(([key, value]) => ({ key, value }));
}

function envVarsToRecord(vars: EnvVar[]): Record<string, string> {
  const result: Record<string, string> = {};
  for (const v of vars) {
    if (v.key.trim()) {
      result[v.key.trim()] = v.value;
    }
  }
  return result;
}

type LoadState = "loading" | "no-settings" | "loaded" | "error";

export function ProjectSettingsTab({
  projectId,
  projectPath,
  hostId,
}: ProjectSettingsTabProps) {
  const navigate = useNavigate();
  const [configuring, setConfiguring] = useState(false);
  const [loadState, setLoadState] = useState<LoadState>("loading");
  const [errorMessage, setErrorMessage] = useState<string>("");
  const [shell, setShell] = useState("");
  const [workingDir, setWorkingDir] = useState("");
  const [envVars, setEnvVars] = useState<EnvVar[]>([]);
  const [autoDetect, setAutoDetect] = useState(true);
  const [defaultPermissions, setDefaultPermissions] = useState("");
  const [autoApprovePatterns, setAutoApprovePatterns] = useState("");
  const [actions, setActions] = useState<ProjectAction[]>([]);
  const [expandedActions, setExpandedActions] = useState<Set<number>>(new Set());
  const [worktreeOnCreate, setWorktreeOnCreate] = useState("");
  const [worktreeOnDelete, setWorktreeOnDelete] = useState("");
  const [linearEnabled, setLinearEnabled] = useState(false);
  const [linearTokenEnvVar, setLinearTokenEnvVar] = useState("LINEAR_TOKEN");
  const [linearTeamKey, setLinearTeamKey] = useState("");
  const [linearProjectId, setLinearProjectId] = useState("");
  const [linearMyEmail, setLinearMyEmail] = useState("");
  const [linearActions, setLinearActions] = useState<LinearAction[]>([]);
  const [tokenValid, setTokenValid] = useState<boolean | null>(null);
  const [tokenUserName, setTokenUserName] = useState<string | null>(null);
  const [validating, setValidating] = useState(false);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);
  const initialRef = useRef<string>("");

  const applySettings = useCallback((settings: ProjectSettings) => {
    setShell(settings.shell ?? "");
    setWorkingDir(settings.working_dir ?? "");
    setEnvVars(settingsToEnvVars(settings));
    setAutoDetect(settings.agentic.auto_detect);
    setDefaultPermissions(settings.agentic.default_permissions.join(", "));
    setAutoApprovePatterns(settings.agentic.auto_approve_patterns.join(", "));
    setActions(settings.actions ?? []);
    setWorktreeOnCreate(settings.worktree?.on_create ?? "");
    setWorktreeOnDelete(settings.worktree?.on_delete ?? "");
    if (settings.linear) {
      setLinearEnabled(true);
      setLinearTokenEnvVar(settings.linear.token_env_var || "LINEAR_TOKEN");
      setLinearTeamKey(settings.linear.team_key || "");
      setLinearProjectId(settings.linear.project_id ?? "");
      setLinearMyEmail(settings.linear.my_email ?? "");
      setLinearActions(settings.linear.actions ?? []);
    } else {
      setLinearEnabled(false);
      setLinearTokenEnvVar("LINEAR_TOKEN");
      setLinearTeamKey("");
      setLinearProjectId("");
      setLinearMyEmail("");
      setLinearActions([]);
    }
    initialRef.current = JSON.stringify(settings);
    setDirty(false);
  }, []);

  const buildSettings = useCallback((): ProjectSettings => {
    const agentic: AgenticSettings = {
      auto_detect: autoDetect,
      default_permissions: defaultPermissions
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean),
      auto_approve_patterns: autoApprovePatterns
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean),
    };
    const validActions = actions.filter((a) => a.name.trim() && a.command.trim());
    const worktree: WorktreeSettings | undefined =
      worktreeOnCreate.trim() || worktreeOnDelete.trim()
        ? {
            on_create: worktreeOnCreate.trim() || undefined,
            on_delete: worktreeOnDelete.trim() || undefined,
          }
        : undefined;
    const linear = linearEnabled
      ? {
          token_env_var: linearTokenEnvVar.trim() || "LINEAR_TOKEN",
          team_key: linearTeamKey.trim(),
          project_id: linearProjectId.trim() || undefined,
          my_email: linearMyEmail.trim() || undefined,
          actions: linearActions.filter((a) => a.name.trim() && a.prompt.trim()),
        }
      : undefined;
    return {
      shell: shell.trim() || undefined,
      working_dir: workingDir.trim() || undefined,
      env: envVarsToRecord(envVars),
      agentic,
      actions: validActions.length > 0 ? validActions : undefined,
      worktree,
      linear,
    };
  }, [shell, workingDir, envVars, autoDetect, defaultPermissions, autoApprovePatterns, actions, worktreeOnCreate, worktreeOnDelete, linearEnabled, linearTokenEnvVar, linearTeamKey, linearProjectId, linearMyEmail, linearActions]);

  const checkDirty = useCallback(() => {
    const current = JSON.stringify(buildSettings());
    setDirty(current !== initialRef.current);
  }, [buildSettings]);

  useEffect(() => {
    checkDirty();
  }, [checkDirty]);

  // Unsaved changes guard
  useEffect(() => {
    if (!dirty) return;
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault();
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, [dirty]);

  const loadSettings = useCallback(async () => {
    setLoadState("loading");
    try {
      const result = await api.projects.getSettings(projectId);
      if (result.error && !result.settings) {
        setErrorMessage(result.error);
        setLoadState("error");
        return;
      }
      if (!result.settings) {
        setLoadState("no-settings");
        return;
      }
      applySettings(result.settings);
      setLoadState("loaded");
    } catch (e) {
      setErrorMessage(e instanceof Error ? e.message : String(e));
      setLoadState("error");
    }
  }, [projectId, applySettings]);

  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);

  const handleSave = useCallback(async () => {
    setSaving(true);
    try {
      const settings = buildSettings();
      await api.projects.saveSettings(projectId, settings);
      initialRef.current = JSON.stringify(settings);
      setDirty(false);
      showToast("Settings saved", "success");
    } catch (e) {
      console.error("failed to save settings", e);
      showToast("Failed to save settings", "error");
    } finally {
      setSaving(false);
    }
  }, [projectId, buildSettings]);

  const handleCreate = useCallback(async () => {
    setSaving(true);
    try {
      const settings = defaultSettings();
      await api.projects.saveSettings(projectId, settings);
      applySettings(settings);
      setLoadState("loaded");
      showToast("Settings created", "success");
    } catch (e) {
      console.error("failed to create settings", e);
      showToast("Failed to create settings", "error");
    } finally {
      setSaving(false);
    }
  }, [projectId, applySettings]);

  const handleConfigureWithClaude = useCallback(async () => {
    setConfiguring(true);
    try {
      const task = await api.projects.configureWithClaude(projectId);
      void navigate(`/hosts/${hostId}/sessions/${task.session_id}`);
    } catch (e) {
      console.error("failed to start configuration", e);
      showToast("Failed to start configuration", "error");
    } finally {
      setConfiguring(false);
    }
  }, [projectId, hostId, navigate]);

  const handleReset = useCallback(async () => {
    setSaving(true);
    try {
      const settings = defaultSettings();
      await api.projects.saveSettings(projectId, settings);
      applySettings(settings);
      setLoadState("loaded");
      showToast("Settings reset to defaults", "success");
    } catch (e) {
      console.error("failed to reset settings", e);
      showToast("Failed to reset settings", "error");
    } finally {
      setSaving(false);
    }
  }, [projectId, applySettings]);

  const handleAddAction = useCallback(() => {
    setActions((prev) => [...prev, emptyAction()]);
  }, []);

  const handleRemoveAction = useCallback((index: number) => {
    setActions((prev) => prev.filter((_, i) => i !== index));
    setExpandedActions((prev) => {
      const next = new Set(prev);
      next.delete(index);
      return next;
    });
  }, []);

  const handleActionChange = useCallback(
    (index: number, field: keyof ProjectAction, val: unknown) => {
      setActions((prev) =>
        prev.map((a, i) => (i === index ? { ...a, [field]: val } : a)),
      );
    },
    [],
  );

  const toggleActionExpanded = useCallback((index: number) => {
    setExpandedActions((prev) => {
      const next = new Set(prev);
      if (next.has(index)) {
        next.delete(index);
      } else {
        next.add(index);
      }
      return next;
    });
  }, []);

  const handleAddEnvVar = useCallback(() => {
    setEnvVars((prev) => [...prev, { key: "", value: "" }]);
  }, []);

  const handleRemoveEnvVar = useCallback((index: number) => {
    setEnvVars((prev) => prev.filter((_, i) => i !== index));
  }, []);

  const handleEnvVarChange = useCallback(
    (index: number, field: "key" | "value", val: string) => {
      setEnvVars((prev) =>
        prev.map((v, i) => (i === index ? { ...v, [field]: val } : v)),
      );
    },
    [],
  );

  const handleValidateToken = useCallback(async () => {
    setValidating(true);
    setTokenValid(null);
    setTokenUserName(null);
    try {
      const user = await api.linear.me(projectId);
      setTokenValid(true);
      setTokenUserName(user.displayName || user.name);
    } catch {
      setTokenValid(false);
    } finally {
      setValidating(false);
    }
  }, [projectId]);

  const handleAddLinearAction = useCallback(() => {
    setLinearActions((prev) => [...prev, { name: "", prompt: "" }]);
  }, []);

  const handleRemoveLinearAction = useCallback((index: number) => {
    setLinearActions((prev) => prev.filter((_, i) => i !== index));
  }, []);

  const handleLinearActionChange = useCallback(
    (index: number, field: keyof LinearAction, val: string | undefined) => {
      setLinearActions((prev) =>
        prev.map((a, i) => (i === index ? { ...a, [field]: val } : a)),
      );
    },
    [],
  );

  if (loadState === "loading") {
    return (
      <div className="flex items-center gap-2 text-sm text-text-secondary">
        <Loader2 size={16} className="animate-spin" />
        Loading settings...
      </div>
    );
  }

  if (loadState === "no-settings") {
    return (
      <div className="space-y-4">
        <div className="rounded-md border border-border bg-bg-secondary p-6 text-center">
          <p className="mb-4 text-sm text-text-secondary">
            No project settings found. Create a{" "}
            <code className="rounded bg-bg-active px-1 py-0.5 text-xs">.zremote/settings.json</code>{" "}
            file to configure this project.
          </p>
          <div className="flex items-center justify-center gap-3">
            <Button onClick={() => void handleCreate()} disabled={saving}>
              {saving && <Loader2 size={14} className="animate-spin" />}
              Create Settings
            </Button>
            <Button
              onClick={() => void handleConfigureWithClaude()}
              variant="secondary"
              size="sm"
              disabled={configuring}
            >
              <Bot size={14} />
              {configuring ? "Starting..." : "Configure with Claude"}
            </Button>
          </div>
        </div>
      </div>
    );
  }

  if (loadState === "error") {
    return (
      <div className="space-y-4">
        <div className="rounded-md border border-status-error/30 bg-status-error/5 p-4">
          <p className="mb-2 text-sm font-medium text-status-error">
            Failed to load settings
          </p>
          <p className="mb-4 text-xs text-text-secondary">{errorMessage}</p>
          <Button onClick={() => void handleReset()} variant="danger" disabled={saving}>
            {saving && <Loader2 size={14} className="animate-spin" />}
            Reset to defaults
          </Button>
        </div>
      </div>
    );
  }

  const envNameErrors = envVars.map((v) =>
    v.key.trim() !== "" && !ENV_NAME_REGEX.test(v.key)
      ? "Invalid name: use letters, digits, underscore"
      : "",
  );

  return (
    <div className="space-y-6">
      {/* General */}
      <section>
        <h2 className="mb-3 text-sm font-medium text-text-primary">General</h2>
        <div className="space-y-3 rounded-md border border-border bg-bg-secondary p-4">
          <Input
            label="Shell"
            value={shell}
            onChange={(e) => setShell(e.target.value)}
            placeholder="System default"
          />
          <Input
            label="Working directory"
            value={workingDir}
            onChange={(e) => setWorkingDir(e.target.value)}
            placeholder={projectPath}
          />
        </div>
      </section>

      {/* Environment Variables */}
      <section>
        <h2 className="mb-3 text-sm font-medium text-text-primary">
          Environment Variables
        </h2>
        <div className="space-y-2 rounded-md border border-border bg-bg-secondary p-4">
          {envVars.length === 0 && (
            <p className="text-xs text-text-tertiary">
              No environment variables configured.
            </p>
          )}
          {envVars.map((v, i) => (
            <div key={i}>
              <div className="flex items-center gap-2">
                <input
                  type="text"
                  value={v.key}
                  onChange={(e) => handleEnvVarChange(i, "key", e.target.value)}
                  placeholder="NAME"
                  className="h-8 w-40 rounded-md border border-border bg-bg-tertiary px-2 font-mono text-xs text-text-primary placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                  aria-label="Variable name"
                />
                <span className="text-xs text-text-tertiary">=</span>
                <input
                  type="text"
                  value={v.value}
                  onChange={(e) =>
                    handleEnvVarChange(i, "value", e.target.value)
                  }
                  placeholder="value"
                  className="h-8 flex-1 rounded-md border border-border bg-bg-tertiary px-2 font-mono text-xs text-text-primary placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                  aria-label="Variable value"
                />
                <button
                  onClick={() => handleRemoveEnvVar(i)}
                  className="flex h-8 w-8 items-center justify-center rounded-md text-text-tertiary transition-colors hover:bg-bg-hover hover:text-status-error"
                  aria-label="Remove variable"
                >
                  <Trash2 size={14} />
                </button>
              </div>
              {envNameErrors[i] && (
                <p className="mt-1 ml-0 text-xs text-status-error">
                  {envNameErrors[i]}
                </p>
              )}
            </div>
          ))}
          <Button onClick={handleAddEnvVar} variant="ghost" size="sm">
            <Plus size={14} />
            Add variable
          </Button>
        </div>
      </section>

      {/* Agentic */}
      <section>
        <h2 className="mb-3 text-sm font-medium text-text-primary">Agentic</h2>
        <div className="space-y-3 rounded-md border border-border bg-bg-secondary p-4">
          <label className="flex items-center gap-2 text-sm text-text-secondary">
            <input
              type="checkbox"
              checked={autoDetect}
              onChange={(e) => setAutoDetect(e.target.checked)}
              className="rounded border-border"
            />
            Auto-detect agentic loops
          </label>
          <Input
            label="Default permissions"
            value={defaultPermissions}
            onChange={(e) => setDefaultPermissions(e.target.value)}
            placeholder="Read, Glob, Grep"
          />
          <Input
            label="Auto-approve patterns"
            value={autoApprovePatterns}
            onChange={(e) => setAutoApprovePatterns(e.target.value)}
            placeholder="cargo test*, bun run test*"
          />
        </div>
      </section>

      {/* Actions */}
      <section>
        <h2 className="mb-3 text-sm font-medium text-text-primary">Actions</h2>
        <div className="space-y-3 rounded-md border border-border bg-bg-secondary p-4">
          {actions.length === 0 && (
            <p className="text-xs text-text-tertiary">
              No actions configured.
            </p>
          )}
          {actions.map((action, i) => (
            <div
              key={i}
              className="rounded-md border border-border bg-bg-tertiary p-3"
            >
              <div className="flex items-start justify-between gap-2">
                <div className="flex-1 space-y-2">
                  <div className="flex items-center gap-2">
                    <input
                      type="text"
                      value={action.name}
                      onChange={(e) =>
                        handleActionChange(i, "name", e.target.value)
                      }
                      placeholder="Action name"
                      className="h-8 flex-1 rounded-md border border-border bg-bg-secondary px-2 text-sm text-text-primary placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                      aria-label="Action name"
                    />
                    <label className="flex items-center gap-1.5 text-xs text-text-secondary">
                      <input
                        type="checkbox"
                        checked={action.worktree_scoped}
                        onChange={(e) =>
                          handleActionChange(
                            i,
                            "worktree_scoped",
                            e.target.checked,
                          )
                        }
                        className="rounded border-border"
                      />
                      Worktree
                    </label>
                  </div>
                  <input
                    type="text"
                    value={action.command}
                    onChange={(e) =>
                      handleActionChange(i, "command", e.target.value)
                    }
                    placeholder="Command to run"
                    className="h-8 w-full rounded-md border border-border bg-bg-secondary px-2 font-mono text-xs text-text-primary placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                    aria-label="Action command"
                  />
                  <input
                    type="text"
                    value={action.description ?? ""}
                    onChange={(e) =>
                      handleActionChange(
                        i,
                        "description",
                        e.target.value || undefined,
                      )
                    }
                    placeholder="Description (optional)"
                    className="h-8 w-full rounded-md border border-border bg-bg-secondary px-2 text-xs text-text-primary placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                    aria-label="Action description"
                  />
                </div>
                <button
                  onClick={() => handleRemoveAction(i)}
                  className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-text-tertiary transition-colors hover:bg-bg-hover hover:text-status-error"
                  aria-label="Remove action"
                >
                  <Trash2 size={14} />
                </button>
              </div>

              {/* Advanced fields toggle */}
              <button
                type="button"
                onClick={() => toggleActionExpanded(i)}
                className="mt-2 flex items-center gap-1 text-xs text-text-tertiary transition-colors duration-150 hover:text-text-secondary"
              >
                {expandedActions.has(i) ? (
                  <ChevronDown size={12} />
                ) : (
                  <ChevronRight size={12} />
                )}
                Advanced
              </button>
              {expandedActions.has(i) && (
                <div className="mt-2 space-y-2 border-t border-border pt-2">
                  <input
                    type="text"
                    value={action.icon ?? ""}
                    onChange={(e) =>
                      handleActionChange(
                        i,
                        "icon",
                        e.target.value || undefined,
                      )
                    }
                    placeholder="Icon (e.g. play, rocket, git-branch)"
                    className="h-8 w-full rounded-md border border-border bg-bg-secondary px-2 text-xs text-text-primary placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                    aria-label="Action icon"
                  />
                  <input
                    type="text"
                    value={action.working_dir ?? ""}
                    onChange={(e) =>
                      handleActionChange(
                        i,
                        "working_dir",
                        e.target.value || undefined,
                      )
                    }
                    placeholder="Working directory (optional)"
                    className="h-8 w-full rounded-md border border-border bg-bg-secondary px-2 font-mono text-xs text-text-primary placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                    aria-label="Action working directory"
                  />
                </div>
              )}
            </div>
          ))}
          <Button onClick={handleAddAction} variant="ghost" size="sm">
            <Plus size={14} />
            Add action
          </Button>
        </div>
      </section>

      {/* Worktree Hooks */}
      <section>
        <h2 className="mb-3 text-sm font-medium text-text-primary">
          Worktree Hooks
        </h2>
        <div className="space-y-3 rounded-md border border-border bg-bg-secondary p-4">
          <Input
            label="On Create Hook"
            value={worktreeOnCreate}
            onChange={(e) => setWorktreeOnCreate(e.target.value)}
            placeholder="e.g. bun install"
            className="font-mono"
          />
          <Input
            label="On Delete Hook"
            value={worktreeOnDelete}
            onChange={(e) => setWorktreeOnDelete(e.target.value)}
            placeholder="e.g. cleanup.sh"
            className="font-mono"
          />
          <p className="text-xs text-text-tertiary">
            Template variables:{" "}
            <code className="rounded bg-bg-active px-1 py-0.5">
              {"{{project_path}}"}
            </code>
            ,{" "}
            <code className="rounded bg-bg-active px-1 py-0.5">
              {"{{worktree_path}}"}
            </code>
            ,{" "}
            <code className="rounded bg-bg-active px-1 py-0.5">
              {"{{branch}}"}
            </code>
          </p>
        </div>
      </section>

      {/* Linear Integration */}
      <section>
        <h2 className="mb-3 text-sm font-medium text-text-primary">
          Linear Integration
        </h2>
        <div className="space-y-3 rounded-md border border-border bg-bg-secondary p-4">
          <label className="flex items-center gap-2 text-sm text-text-secondary">
            <input
              type="checkbox"
              checked={linearEnabled}
              onChange={(e) => {
                const enabled = e.target.checked;
                setLinearEnabled(enabled);
                if (enabled && linearActions.length === 0) {
                  setLinearActions(DEFAULT_LINEAR_ACTIONS);
                }
              }}
              className="rounded border-border"
              aria-label="Enable Linear integration"
            />
            Enable Linear integration
          </label>
          {linearEnabled && (
            <div className="space-y-3 border-t border-border pt-3">
              <div className="flex items-end gap-2">
                <div className="flex-1">
                  <Input
                    label="Token environment variable"
                    value={linearTokenEnvVar}
                    onChange={(e) => setLinearTokenEnvVar(e.target.value)}
                    placeholder="LINEAR_TOKEN"
                    className="font-mono"
                  />
                </div>
                <Button
                  onClick={() => void handleValidateToken()}
                  variant="ghost"
                  size="sm"
                  disabled={validating}
                >
                  {validating ? (
                    <Loader2 size={14} className="animate-spin" />
                  ) : tokenValid === true ? (
                    <Check size={14} className="text-status-online" />
                  ) : tokenValid === false ? (
                    <AlertCircle size={14} className="text-status-error" />
                  ) : null}
                  Validate
                </Button>
              </div>
              {tokenValid === true && tokenUserName && (
                <p className="text-xs text-status-online">
                  Authenticated as {tokenUserName}
                </p>
              )}
              {tokenValid === false && (
                <p className="text-xs text-status-error">
                  Token validation failed. Check the environment variable.
                </p>
              )}
              <Input
                label="Team key"
                value={linearTeamKey}
                onChange={(e) => setLinearTeamKey(e.target.value)}
                placeholder="e.g. ENG"
              />
              <Input
                label="Project ID (optional)"
                value={linearProjectId}
                onChange={(e) => setLinearProjectId(e.target.value)}
                placeholder="Optional - scope to a specific project"
              />
              <Input
                label="My email (for 'My Issues' filter)"
                value={linearMyEmail}
                onChange={(e) => setLinearMyEmail(e.target.value)}
                placeholder="user@example.com"
              />
              <div>
                <span className="mb-2 block text-xs font-medium text-text-secondary">
                  Actions
                </span>
                <div className="space-y-2">
                  {linearActions.map((action, i) => (
                    <div
                      key={i}
                      className="rounded-md border border-border bg-bg-tertiary p-3"
                    >
                      <div className="flex items-start gap-2">
                        <div className="flex-1 space-y-2">
                          <div className="flex gap-2">
                            <input
                              type="text"
                              value={action.name}
                              onChange={(e) =>
                                handleLinearActionChange(i, "name", e.target.value)
                              }
                              placeholder="Action name"
                              className="h-8 flex-1 rounded-md border border-border bg-bg-secondary px-2 text-sm text-text-primary placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                              aria-label="Linear action name"
                            />
                            <input
                              type="text"
                              value={action.icon ?? ""}
                              onChange={(e) =>
                                handleLinearActionChange(
                                  i,
                                  "icon",
                                  e.target.value || undefined,
                                )
                              }
                              placeholder="Icon"
                              className="h-8 w-24 rounded-md border border-border bg-bg-secondary px-2 text-xs text-text-primary placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                              aria-label="Linear action icon"
                            />
                          </div>
                          <textarea
                            value={action.prompt}
                            onChange={(e) =>
                              handleLinearActionChange(i, "prompt", e.target.value)
                            }
                            placeholder="Prompt template (use {{issue.identifier}}, {{issue.title}}, {{issue.description}})"
                            rows={3}
                            className="w-full rounded-md border border-border bg-bg-secondary px-2 py-1.5 text-xs text-text-primary placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                            aria-label="Linear action prompt"
                          />
                        </div>
                        <button
                          onClick={() => handleRemoveLinearAction(i)}
                          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-text-tertiary transition-colors hover:bg-bg-hover hover:text-status-error"
                          aria-label="Remove linear action"
                        >
                          <Trash2 size={14} />
                        </button>
                      </div>
                    </div>
                  ))}
                  <Button onClick={handleAddLinearAction} variant="ghost" size="sm">
                    <Plus size={14} />
                    Add action
                  </Button>
                </div>
              </div>
            </div>
          )}
        </div>
      </section>

      {/* Save */}
      <div className="flex items-center gap-3">
        <Button
          onClick={() => void handleSave()}
          disabled={!dirty || saving}
        >
          {saving && <Loader2 size={14} className="animate-spin" />}
          Save Settings
        </Button>
        {dirty && (
          <span className="text-xs text-text-tertiary">Unsaved changes</span>
        )}
      </div>
    </div>
  );
}
