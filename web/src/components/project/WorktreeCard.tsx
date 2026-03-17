import { GitBranch, Terminal, Trash2 } from "lucide-react";
import type { Project, ProjectAction } from "../../lib/api";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import { ActionCard } from "./ActionCard";

interface WorktreeCardProps {
  worktree: Project;
  parentProjectId: string;
  hostId: string;
  worktreeActions: ProjectAction[];
  onDelete: () => void;
  onOpenTerminal: () => void;
}

export function WorktreeCard({
  worktree,
  parentProjectId,
  hostId,
  worktreeActions,
  onDelete,
  onOpenTerminal,
}: WorktreeCardProps) {
  return (
    <div className="rounded-lg border border-border bg-bg-secondary p-4">
      <div className="flex items-start justify-between">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <GitBranch size={14} className="shrink-0 text-text-tertiary" />
            <span className="truncate text-sm font-medium text-text-primary">
              {worktree.git_branch ?? worktree.name}
            </span>
            {worktree.git_commit_hash && (
              <span className="shrink-0 font-mono text-xs text-text-tertiary">
                {worktree.git_commit_hash.slice(0, 7)}
              </span>
            )}
            {worktree.git_is_dirty && (
              <Badge variant="warning">Modified</Badge>
            )}
          </div>
          {worktree.git_commit_message && (
            <p className="mt-1 truncate text-sm text-text-secondary">
              {worktree.git_commit_message}
            </p>
          )}
          <p className="mt-1 truncate font-mono text-xs text-text-tertiary">
            {worktree.path}
          </p>
        </div>
        <div className="ml-4 flex shrink-0 items-center gap-1">
          <Button
            onClick={onOpenTerminal}
            variant="ghost"
            size="sm"
            aria-label="Open terminal"
          >
            <Terminal size={14} />
          </Button>
          <Button
            onClick={onDelete}
            variant="ghost"
            size="sm"
            aria-label="Delete worktree"
          >
            <Trash2 size={14} />
          </Button>
        </div>
      </div>

      {worktreeActions.length > 0 && (
        <div className="mt-3 border-t border-border pt-3">
          <h3 className="mb-2 text-xs font-medium text-text-tertiary">
            Actions
          </h3>
          <div className="grid grid-cols-1 gap-2">
            {worktreeActions.map((action) => (
              <ActionCard
                key={action.name}
                action={action}
                projectId={parentProjectId}
                hostId={hostId}
                worktreePath={worktree.path}
                worktreeBranch={worktree.git_branch ?? undefined}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
