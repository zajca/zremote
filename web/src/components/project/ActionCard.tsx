import * as LucideIcons from "lucide-react";
import { Loader2, Terminal } from "lucide-react";
import { useCallback, useState } from "react";
import { useNavigate } from "react-router";
import { api, type ProjectAction, type RunActionRequest } from "../../lib/api";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import { showToast } from "../layout/Toast";

function getIcon(name?: string): React.ComponentType<LucideIcons.LucideProps> {
  if (!name) return Terminal;
  const pascalName = name
    .split("-")
    .map((s) => s.charAt(0).toUpperCase() + s.slice(1))
    .join("");
  return (
    ((LucideIcons as Record<string, unknown>)[pascalName] as React.ComponentType<LucideIcons.LucideProps>) ||
    Terminal
  );
}

interface ActionCardProps {
  action: ProjectAction;
  projectId: string;
  hostId: string;
  worktreePath?: string;
  worktreeBranch?: string;
}

export function ActionCard({
  action,
  projectId,
  hostId,
  worktreePath,
  worktreeBranch,
}: ActionCardProps) {
  const navigate = useNavigate();
  const [running, setRunning] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const Icon = getIcon(action.icon);

  const handleRun = useCallback(async () => {
    setRunning(true);
    try {
      const body: RunActionRequest = {};
      if (worktreePath) body.worktree_path = worktreePath;
      if (worktreeBranch) body.branch = worktreeBranch;
      const session = await api.projects.runAction(projectId, action.name, body);
      void navigate(`/hosts/${hostId}/sessions/${session.id}`);
    } catch (e) {
      console.error("failed to run action", e);
      showToast(`Failed to run "${action.name}"`, "error");
    } finally {
      setRunning(false);
    }
  }, [projectId, action.name, hostId, worktreePath, worktreeBranch, navigate]);

  return (
    <div className="rounded-lg border border-border bg-bg-secondary p-4">
      <div className="mb-2 flex items-start justify-between">
        <div className="flex items-center gap-2">
          <Icon size={16} className="shrink-0 text-text-secondary" />
          <span className="text-sm font-medium text-text-primary">
            {action.name}
          </span>
          {action.worktree_scoped && (
            <Badge variant="creating">worktree</Badge>
          )}
        </div>
      </div>
      {action.description && (
        <p className="mb-2 text-xs text-text-secondary">{action.description}</p>
      )}
      <button
        type="button"
        onClick={() => setExpanded(!expanded)}
        className={`mb-3 block w-full text-left font-mono text-xs text-text-tertiary transition-colors duration-150 hover:text-text-secondary ${
          expanded ? "whitespace-pre-wrap break-all" : "truncate"
        }`}
      >
        {action.command}
      </button>
      <Button
        onClick={() => void handleRun()}
        disabled={running}
        size="sm"
        variant="secondary"
      >
        {running ? (
          <Loader2 size={14} className="animate-spin" />
        ) : (
          <Terminal size={14} />
        )}
        Run
      </Button>
    </div>
  );
}
