import { Loader2, Plus, Trash2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type AgenticSettings, type ProjectSettings } from "../lib/api";
import { Button } from "./ui/Button";
import { Input } from "./ui/Input";
import { showToast } from "./layout/Toast";

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

function defaultSettings(): ProjectSettings {
  return {
    env: {},
    agentic: {
      auto_detect: true,
      default_permissions: [],
      auto_approve_patterns: [],
    },
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
}: ProjectSettingsTabProps) {
  const [loadState, setLoadState] = useState<LoadState>("loading");
  const [errorMessage, setErrorMessage] = useState<string>("");
  const [shell, setShell] = useState("");
  const [workingDir, setWorkingDir] = useState("");
  const [envVars, setEnvVars] = useState<EnvVar[]>([]);
  const [autoDetect, setAutoDetect] = useState(true);
  const [defaultPermissions, setDefaultPermissions] = useState("");
  const [autoApprovePatterns, setAutoApprovePatterns] = useState("");
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
    return {
      shell: shell.trim() || undefined,
      working_dir: workingDir.trim() || undefined,
      env: envVarsToRecord(envVars),
      agentic,
    };
  }, [shell, workingDir, envVars, autoDetect, defaultPermissions, autoApprovePatterns]);

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
          <Button onClick={() => void handleCreate()} disabled={saving}>
            {saving && <Loader2 size={14} className="animate-spin" />}
            Create Settings
          </Button>
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
