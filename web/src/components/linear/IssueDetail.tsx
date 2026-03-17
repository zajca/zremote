import { ExternalLink, Loader2 } from "lucide-react";
import { useState } from "react";
import { api } from "../../lib/api";
import type { LinearAction, LinearIssue } from "../../types/linear";
import { Button } from "../ui/Button";

interface IssueDetailProps {
  issue: LinearIssue;
  projectId: string;
  actions: LinearAction[];
  onStartClaude: (prompt: string) => void;
}

const PRIORITY_LABELS: Record<number, string> = {
  0: "None",
  1: "Urgent",
  2: "High",
  3: "Medium",
  4: "Low",
};

export function IssueDetail({
  issue,
  projectId,
  actions,
  onStartClaude,
}: IssueDetailProps) {
  const [executingAction, setExecutingAction] = useState<number | null>(null);

  const handleAction = async (actionIndex: number) => {
    setExecutingAction(actionIndex);
    try {
      const result = await api.linear.executeAction(
        projectId,
        actionIndex,
        issue.id,
      );
      onStartClaude(result.prompt);
    } catch (err) {
      console.error("Failed to execute action", err);
    } finally {
      setExecutingAction(null);
    }
  };

  return (
    <div className="rounded-md border border-border bg-bg-secondary p-4">
      <div className="mb-3 flex items-start justify-between">
        <div>
          <h3 className="text-sm font-semibold text-text-primary">
            <span className="font-mono text-accent">{issue.identifier}</span>
            {": "}
            {issue.title}
          </h3>
        </div>
        <a
          href={issue.url}
          target="_blank"
          rel="noopener noreferrer"
          className="flex items-center gap-1 text-xs text-text-tertiary transition-colors hover:text-accent"
          aria-label="Open in Linear"
        >
          <ExternalLink size={12} />
          Open
        </a>
      </div>

      <div className="mb-3 flex flex-wrap items-center gap-3 text-xs text-text-secondary">
        <span className="flex items-center gap-1">
          <span
            className="inline-block h-2 w-2 rounded-full"
            style={{ backgroundColor: issue.state.color }}
          />
          {issue.state.name}
        </span>
        {issue.assignee && (
          <span>{issue.assignee.displayName || issue.assignee.name}</span>
        )}
        <span>{PRIORITY_LABELS[issue.priority] ?? "Unknown"} priority</span>
        {issue.cycle && (
          <span>{issue.cycle.name ?? `Cycle ${issue.cycle.number}`}</span>
        )}
        {issue.labels.nodes.length > 0 && (
          <div className="flex gap-1">
            {issue.labels.nodes.map((label) => (
              <span
                key={label.id}
                className="rounded-full px-1.5 py-0.5"
                style={{
                  backgroundColor: `${label.color}20`,
                  color: label.color,
                }}
              >
                {label.name}
              </span>
            ))}
          </div>
        )}
      </div>

      {issue.description && (
        <div className="mb-4 whitespace-pre-wrap text-sm text-text-secondary">
          {issue.description}
        </div>
      )}

      {actions.length > 0 && (
        <div className="flex flex-wrap gap-2 border-t border-border pt-3">
          {actions.map((action, i) => (
            <Button
              key={i}
              onClick={() => void handleAction(i)}
              variant="secondary"
              size="sm"
              disabled={executingAction !== null}
            >
              {executingAction === i && (
                <Loader2 size={12} className="animate-spin" />
              )}
              {action.name}
            </Button>
          ))}
        </div>
      )}
    </div>
  );
}
