import { AlertCircle, ListTodo, Loader2, RefreshCw, Settings } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { api } from "../../lib/api";
import type { IssuePreset, LinearAction, LinearIssue } from "../../types/linear";
import { Button } from "../ui/Button";
import { StartClaudeDialog } from "../StartClaudeDialog";
import { IssueDetail } from "./IssueDetail";
import { IssueFilterBar } from "./IssueFilterBar";
import { IssueRow } from "./IssueRow";

interface LinearIssuesPanelProps {
  projectId: string;
  hostId: string;
}

type PanelState = "loading" | "not-configured" | "error" | "empty" | "data";

export function LinearIssuesPanel({ projectId, hostId }: LinearIssuesPanelProps) {
  const [panelState, setPanelState] = useState<PanelState>("loading");
  const [issues, setIssues] = useState<LinearIssue[]>([]);
  const [actions, setActions] = useState<LinearAction[]>([]);
  const [selectedIssue, setSelectedIssue] = useState<LinearIssue | null>(null);
  const [activePreset, setActivePreset] = useState<IssuePreset | null>("my_issues");
  const [errorMessage, setErrorMessage] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [projectName, setProjectName] = useState("");
  const [projectPath, setProjectPath] = useState("");

  // Claude dialog state
  const [claudePrompt, setClaudePrompt] = useState<string | null>(null);

  const fetchIssues = useCallback(async (preset: IssuePreset | null) => {
    try {
      const data = await api.linear.issues(projectId, {
        preset: preset ?? undefined,
      });
      setIssues(data);
      setPanelState(data.length === 0 ? "empty" : "data");
      setSelectedIssue(null);
    } catch (err) {
      setErrorMessage(err instanceof Error ? err.message : "Failed to fetch issues");
      setPanelState("error");
    }
  }, [projectId]);

  const loadSettings = useCallback(async () => {
    setPanelState("loading");
    try {
      const result = await api.projects.getSettings(projectId);
      if (!result.settings?.linear) {
        setPanelState("not-configured");
        return;
      }
      setActions(result.settings.linear.actions ?? []);
      await fetchIssues(activePreset);
    } catch (err) {
      setErrorMessage(err instanceof Error ? err.message : "Failed to load settings");
      setPanelState("error");
    }
  }, [projectId, fetchIssues, activePreset]);

  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);

  useEffect(() => {
    void api.projects.get(projectId).then(
      (p) => {
        setProjectName(p.name);
        setProjectPath(p.path);
      },
      () => {},
    );
  }, [projectId]);

  const handlePresetChange = useCallback(
    (preset: IssuePreset | null) => {
      setActivePreset(preset);
      setRefreshing(true);
      void fetchIssues(preset).finally(() => setRefreshing(false));
    },
    [fetchIssues],
  );

  const handleRetry = useCallback(() => {
    void loadSettings();
  }, [loadSettings]);

  const handleStartClaude = useCallback((prompt: string) => {
    setClaudePrompt(prompt);
  }, []);

  if (panelState === "loading") {
    return (
      <div className="space-y-3">
        {[1, 2, 3, 4].map((i) => (
          <div
            key={i}
            className="h-10 animate-pulse rounded-md bg-bg-secondary"
          />
        ))}
      </div>
    );
  }

  if (panelState === "not-configured") {
    return (
      <div className="flex flex-col items-center gap-4 pt-24 text-center">
        <Settings size={32} className="text-text-tertiary" />
        <p className="text-sm text-text-secondary">
          Linear integration is not configured for this project.
        </p>
        <p className="text-xs text-text-tertiary">
          Go to the Settings tab to enable Linear integration.
        </p>
      </div>
    );
  }

  if (panelState === "error") {
    return (
      <div className="flex flex-col items-center gap-4 pt-24 text-center">
        <AlertCircle size={32} className="text-status-error" />
        <p className="text-sm text-text-secondary">
          Failed to load Linear issues
        </p>
        <p className="text-xs text-text-tertiary">{errorMessage}</p>
        <Button onClick={handleRetry} variant="secondary" size="sm">
          <RefreshCw size={14} />
          Retry
        </Button>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <IssueFilterBar
          activePreset={activePreset}
          onPresetChange={handlePresetChange}
        />
        {refreshing && (
          <Loader2 size={14} className="animate-spin text-text-tertiary" />
        )}
      </div>

      {panelState === "empty" ? (
        <div className="flex flex-col items-center gap-4 pt-24 text-center">
          <ListTodo size={32} className="text-text-tertiary" />
          <p className="text-sm text-text-secondary">
            No issues match your filters
          </p>
          <p className="text-xs text-text-tertiary">
            Try a different filter or check your Linear configuration.
          </p>
        </div>
      ) : (
        <div className="space-y-1">
          {issues.map((issue) => (
            <IssueRow
              key={issue.id}
              issue={issue}
              isSelected={selectedIssue?.id === issue.id}
              onClick={() =>
                setSelectedIssue(
                  selectedIssue?.id === issue.id ? null : issue,
                )
              }
            />
          ))}
        </div>
      )}

      {selectedIssue && (
        <IssueDetail
          issue={selectedIssue}
          projectId={projectId}
          actions={actions}
          onStartClaude={handleStartClaude}
        />
      )}

      {claudePrompt !== null && (
        <StartClaudeDialog
          projectName={projectName}
          projectPath={projectPath}
          hostId={hostId}
          projectId={projectId}
          initialPrompt={claudePrompt}
          onClose={() => setClaudePrompt(null)}
        />
      )}
    </div>
  );
}
