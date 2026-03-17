import { Zap } from "lucide-react";
import { useEffect, useState } from "react";
import { api, type ProjectAction } from "../../lib/api";
import { ActionCard } from "./ActionCard";

interface ActionsTabProps {
  projectId: string;
  projectPath: string;
  hostId: string;
}

export function ActionsTab({ projectId, hostId }: ActionsTabProps) {
  const [actions, setActions] = useState<ProjectAction[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    setLoading(true);
    void api.projects.actions(projectId).then(
      (res) => {
        setActions(res.actions);
        setLoading(false);
      },
      () => {
        setActions([]);
        setLoading(false);
      },
    );
  }, [projectId]);

  if (loading) {
    return (
      <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
        {[0, 1, 2].map((i) => (
          <div
            key={i}
            className="h-32 animate-pulse rounded-lg border border-border bg-bg-tertiary"
          />
        ))}
      </div>
    );
  }

  if (actions.length === 0) {
    return (
      <div className="flex flex-col items-center gap-4 pt-24 text-center">
        <Zap size={32} className="text-text-secondary" />
        <div>
          <p className="text-sm text-text-secondary">No actions configured</p>
          <p className="mt-1 text-xs text-text-tertiary">
            Define actions in{" "}
            <code className="rounded bg-bg-active px-1 py-0.5 text-xs">
              .zremote/settings.json
            </code>
          </p>
        </div>
      </div>
    );
  }

  const projectActions = actions.filter((a) => !a.worktree_scoped);
  const worktreeActions = actions.filter((a) => a.worktree_scoped);

  return (
    <div className="space-y-6">
      {projectActions.length > 0 && (
        <div>
          <h2 className="mb-3 text-sm font-medium text-text-primary">
            Project Actions
          </h2>
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            {projectActions.map((action) => (
              <ActionCard
                key={action.name}
                action={action}
                projectId={projectId}
                hostId={hostId}
              />
            ))}
          </div>
        </div>
      )}
      {worktreeActions.length > 0 && (
        <div>
          <h2 className="mb-3 text-sm font-medium text-text-primary">
            Worktree Actions
          </h2>
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            {worktreeActions.map((action) => (
              <ActionCard
                key={action.name}
                action={action}
                projectId={projectId}
                hostId={hostId}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
