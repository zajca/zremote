import { Loader2, Play } from "lucide-react";
import { useCallback, useState } from "react";
import { useNavigate } from "react-router";
import { api, type ProjectAction, type RunActionRequest } from "../../lib/api";
import { Badge } from "../ui/Badge";
import { IconButton } from "../ui/IconButton";
import { showToast } from "../layout/Toast";
import { ActionInputPopover } from "./ActionInputPopover";
import { detectMissingInputs, getActionIcon } from "./action-utils";

interface ActionRowProps {
  action: ProjectAction;
  projectId: string;
  hostId: string;
  worktreePath?: string;
  worktreeBranch?: string;
}

export function ActionRow({
  action,
  projectId,
  hostId,
  worktreePath,
  worktreeBranch,
}: ActionRowProps) {
  const navigate = useNavigate();
  const [running, setRunning] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [showInput, setShowInput] = useState(false);
  const Icon = getActionIcon(action.icon);

  const runAction = useCallback(
    async (body: RunActionRequest) => {
      setRunning(true);
      try {
        const result = await api.projects.runAction(projectId, action.name, body);
        void navigate(`/hosts/${hostId}/sessions/${result.session_id}`);
      } catch (e) {
        console.error("failed to run action", e);
        showToast(`Failed to run "${action.name}"`, "error");
      } finally {
        setRunning(false);
      }
    },
    [projectId, action.name, hostId, navigate],
  );

  const handleRun = useCallback(() => {
    const { needsWorktree, needsBranch } = detectMissingInputs(
      action.command,
      action.working_dir,
      worktreePath,
      worktreeBranch,
    );

    if (needsWorktree || needsBranch) {
      setShowInput(true);
      return;
    }

    const body: RunActionRequest = {};
    if (worktreePath) body.worktree_path = worktreePath;
    if (worktreeBranch) body.branch = worktreeBranch;
    void runAction(body);
  }, [action.command, action.working_dir, worktreePath, worktreeBranch, runAction]);

  const handleRunWithValues = useCallback(
    (values: { worktreePath?: string; branch?: string }) => {
      setShowInput(false);
      const body: RunActionRequest = {};
      if (values.worktreePath) body.worktree_path = values.worktreePath;
      if (values.branch) body.branch = values.branch;
      void runAction(body);
    },
    [runAction],
  );

  const missingInputs = detectMissingInputs(
    action.command,
    action.working_dir,
    worktreePath,
    worktreeBranch,
  );

  return (
    <div>
      <div className="group/action flex items-center gap-2 rounded-md px-3 py-1.5 hover:bg-bg-hover">
        <Icon size={14} className="shrink-0 text-text-secondary" />
        <div className="relative shrink-0">
          <IconButton
            icon={running ? Loader2 : Play}
            onClick={handleRun}
            disabled={running}
            aria-label={`Run ${action.name}`}
            className={running ? "animate-spin" : ""}
          />
          {showInput && (
            <ActionInputPopover
              projectId={projectId}
              needsWorktree={missingInputs.needsWorktree}
              needsBranch={missingInputs.needsBranch}
              onSubmit={handleRunWithValues}
              onCancel={() => setShowInput(false)}
            />
          )}
        </div>
        <span className="shrink-0 text-sm font-medium text-text-primary">
          {action.name}
        </span>
        {action.worktree_scoped && (
          <Badge variant="creating">worktree</Badge>
        )}
        <button
          type="button"
          onClick={() => setExpanded(!expanded)}
          className="min-w-0 flex-1 truncate text-left font-mono text-xs text-text-tertiary transition-colors duration-150 hover:text-text-secondary"
        >
          {action.command}
        </button>
      </div>
      {expanded && (
        <div className="ml-6 px-3 pb-1.5">
          {action.description && (
            <p className="text-xs text-text-secondary">{action.description}</p>
          )}
          <pre className="whitespace-pre-wrap break-all font-mono text-xs text-text-tertiary">
            {action.command}
          </pre>
        </div>
      )}
    </div>
  );
}
