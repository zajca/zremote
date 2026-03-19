import { Settings } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useHosts } from "../../hooks/useHosts";
import { useMode } from "../../hooks/useMode";
import { api } from "../../lib/api";
import { requestBrowserPermission } from "../../lib/browser-notifications";
import { useNotificationStore } from "../../stores/notification-store";
import { showToast } from "../layout/Toast";
import { Toggle } from "../ui/Toggle";

interface ConfigEntry {
  key: string;
  label: string;
  description: string;
}

const GLOBAL_CONFIG_KEYS: ConfigEntry[] = [
  {
    key: "notifications.enabled",
    label: "Notifications",
    description: "Enable browser notifications when Claude needs input",
  },
  {
    key: "auto_approve.enabled",
    label: "Auto-approve tools",
    description: "Automatically approve safe tool calls",
  },
];

export function SettingsPage() {
  const { hosts } = useHosts();
  const { isLocal } = useMode();
  const [selectedHostId, setSelectedHostId] = useState<string>("");
  const [globalValues, setGlobalValues] = useState<Record<string, string>>({});
  const [hostValues, setHostValues] = useState<Record<string, string>>({});
  const [saved, setSaved] = useState(false);

  // Load global config
  useEffect(() => {
    const loadGlobal = async () => {
      const values: Record<string, string> = {};
      for (const entry of GLOBAL_CONFIG_KEYS) {
        try {
          const config = await api.config.getGlobal(entry.key);
          values[entry.key] = config.value;
        } catch {
          // Key not set yet
        }
      }
      setGlobalValues(values);
    };
    void loadGlobal();
  }, []);

  // Load host config when host selected
  useEffect(() => {
    if (!selectedHostId) {
      setHostValues({});
      return;
    }
    const loadHost = async () => {
      const values: Record<string, string> = {};
      for (const entry of GLOBAL_CONFIG_KEYS) {
        try {
          const config = await api.config.getHost(selectedHostId, entry.key);
          values[entry.key] = config.value;
        } catch {
          // Key not set for this host
        }
      }
      setHostValues(values);
    };
    void loadHost();
  }, [selectedHostId]);

  const handleGlobalToggle = useCallback(
    async (key: string) => {
      const current = globalValues[key] === "true";

      if (key === "notifications.enabled") {
        if (!current) {
          const permission = await requestBrowserPermission();
          useNotificationStore.getState().setBrowserPermission(permission);
          if (permission !== "granted") {
            showToast("Browser notifications were not permitted", "info");
            return;
          }
          useNotificationStore.getState().setBrowserEnabled(true);
        } else {
          useNotificationStore.getState().setBrowserEnabled(false);
        }
      }

      const newValue = String(!current);
      try {
        await api.config.setGlobal(key, newValue);
        setGlobalValues((prev) => ({ ...prev, [key]: newValue }));
        setSaved(true);
        setTimeout(() => setSaved(false), 1500);
      } catch (e) {
        console.error("failed to set config", e);
        showToast("Failed to save setting", "error");
      }
    },
    [globalValues],
  );

  const handleHostToggle = useCallback(
    async (key: string) => {
      if (!selectedHostId) return;
      const current = hostValues[key] === "true";
      const newValue = String(!current);
      try {
        await api.config.setHost(selectedHostId, key, newValue);
        setHostValues((prev) => ({ ...prev, [key]: newValue }));
        setSaved(true);
        setTimeout(() => setSaved(false), 1500);
      } catch (e) {
        console.error("failed to set host config", e);
        showToast("Failed to save host setting", "error");
      }
    },
    [selectedHostId, hostValues],
  );

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-3 border-b border-border px-6 py-4">
        <Settings size={20} className="text-accent" />
        <h1 className="text-lg font-semibold text-text-primary">Settings</h1>
        {saved && (
          <span className="text-xs text-accent">Saved</span>
        )}
      </div>

      <div className="flex-1 overflow-auto p-6">
        <div className="mx-auto max-w-2xl space-y-8">
          {/* Global settings */}
          <section>
            <h2 className="mb-4 text-sm font-semibold text-text-primary">
              Global Settings
            </h2>
            <div className="space-y-3">
              {GLOBAL_CONFIG_KEYS.map((entry) => (
                <label
                  key={entry.key}
                  className="flex items-center justify-between rounded-lg border border-border p-4"
                >
                  <div>
                    <div className="text-sm font-medium text-text-primary">
                      {entry.label}
                    </div>
                    <div className="text-xs text-text-tertiary">
                      {entry.description}
                    </div>
                  </div>
                  <Toggle
                    checked={globalValues[entry.key] === "true"}
                    onChange={() => void handleGlobalToggle(entry.key)}
                    aria-label={entry.label}
                  />
                </label>
              ))}
            </div>
          </section>

          {/* Per-host settings - hidden in local mode (single host) */}
          {!isLocal && (
            <section>
              <h2 className="mb-4 text-sm font-semibold text-text-primary">
                Per-Host Overrides
              </h2>
              <select
                value={selectedHostId}
                onChange={(e) => setSelectedHostId(e.target.value)}
                className="mb-4 w-full rounded-lg border border-border bg-bg-secondary px-3 py-2 text-sm text-text-primary"
              >
                <option value="">Select a host...</option>
                {hosts.map((host) => (
                  <option key={host.id} value={host.id}>
                    {host.hostname}
                  </option>
                ))}
              </select>

              {selectedHostId && (
                <div className="space-y-3">
                  {GLOBAL_CONFIG_KEYS.map((entry) => (
                    <label
                      key={entry.key}
                      className="flex items-center justify-between rounded-lg border border-border p-4"
                    >
                      <div>
                        <div className="text-sm font-medium text-text-primary">
                          {entry.label}
                        </div>
                        <div className="text-xs text-text-tertiary">
                          Override for this host
                        </div>
                      </div>
                      <Toggle
                        checked={hostValues[entry.key] === "true"}
                        onChange={() => void handleHostToggle(entry.key)}
                        aria-label={`${entry.label} override`}
                      />
                    </label>
                  ))}
                </div>
              )}
            </section>
          )}
        </div>
      </div>
    </div>
  );
}
