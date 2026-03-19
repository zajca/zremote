import { GitBranch, Loader2, Play, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type Project } from "../../lib/api";
import { Button } from "../ui/Button";
import { Input } from "../ui/Input";

interface ActionInputPopoverProps {
  projectId: string;
  needsWorktree: boolean;
  needsBranch: boolean;
  onSubmit: (values: { worktreePath?: string; branch?: string }) => void;
  onCancel: () => void;
}

export function ActionInputPopover({
  projectId,
  needsWorktree,
  needsBranch,
  onSubmit,
  onCancel,
}: ActionInputPopoverProps) {
  const [worktrees, setWorktrees] = useState<Project[]>([]);
  const [loading, setLoading] = useState(needsWorktree);
  const [selectedWorktree, setSelectedWorktree] = useState("");
  const [branch, setBranch] = useState("");
  const panelRef = useRef<HTMLDivElement>(null);
  const branchInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!needsWorktree) return;
    let cancelled = false;
    void api.projects.worktrees(projectId).then((wts) => {
      if (cancelled) return;
      setWorktrees(wts);
      if (wts.length > 0) setSelectedWorktree(wts[0]!.path);
      setLoading(false);
    }).catch(() => {
      if (!cancelled) setLoading(false);
    });
    return () => { cancelled = true; };
  }, [projectId, needsWorktree]);

  useEffect(() => {
    if (!needsWorktree && needsBranch) {
      branchInputRef.current?.focus();
    }
  }, [needsWorktree, needsBranch]);

  const handleSubmit = useCallback(() => {
    if (needsWorktree) {
      const wt = worktrees.find((w) => w.path === selectedWorktree);
      if (!wt) return;
      onSubmit({ worktreePath: wt.path, branch: wt.git_branch ?? undefined });
    } else if (needsBranch) {
      if (!branch.trim()) return;
      onSubmit({ branch: branch.trim() });
    }
  }, [needsWorktree, needsBranch, worktrees, selectedWorktree, branch, onSubmit]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onCancel();
      } else if (e.key === "Enter") {
        e.stopPropagation();
        handleSubmit();
      }
    },
    [onCancel, handleSubmit],
  );

  const canSubmit = needsWorktree
    ? selectedWorktree !== "" && worktrees.length > 0
    : branch.trim() !== "";

  return (
    <>
      <div
        className="fixed inset-0 z-40"
        onClick={onCancel}
        data-testid="popover-backdrop"
      />
      <div
        ref={panelRef}
        role="dialog"
        aria-label="Action input"
        onKeyDown={handleKeyDown}
        className="absolute top-full left-0 z-50 mt-1 w-64 rounded-lg border border-border bg-bg-primary p-4 shadow-xl"
      >
        {loading ? (
          <div className="flex items-center gap-2 py-2" data-testid="popover-loading">
            <Loader2 size={14} className="animate-spin text-text-tertiary" />
            <span className="text-xs text-text-tertiary">Loading worktrees...</span>
          </div>
        ) : needsWorktree ? (
          worktrees.length === 0 ? (
            <div className="flex flex-col items-center gap-2 py-2" data-testid="popover-empty">
              <GitBranch size={24} className="text-text-tertiary" />
              <span className="text-xs text-text-secondary">No worktrees found</span>
            </div>
          ) : (
            <div className="flex flex-col gap-3">
              <div className="flex flex-col gap-1.5">
                <label htmlFor="worktree-select" className="text-xs font-medium text-text-secondary">
                  Worktree
                </label>
                <select
                  id="worktree-select"
                  value={selectedWorktree}
                  onChange={(e) => setSelectedWorktree(e.target.value)}
                  className="h-8 rounded-md border border-border bg-bg-tertiary px-3 text-sm text-text-primary transition-colors duration-150 focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
                >
                  {worktrees.map((wt) => (
                    <option key={wt.id} value={wt.path}>
                      {wt.git_branch ?? wt.name}
                    </option>
                  ))}
                </select>
              </div>
              <div className="flex items-center justify-end gap-2">
                <Button size="sm" variant="ghost" onClick={onCancel}>
                  <X size={14} />
                  Cancel
                </Button>
                <Button size="sm" variant="primary" onClick={handleSubmit} disabled={!canSubmit}>
                  <Play size={14} />
                  Run
                </Button>
              </div>
            </div>
          )
        ) : (
          <div className="flex flex-col gap-3">
            <Input
              ref={branchInputRef}
              id="branch-input"
              label="Branch"
              placeholder="e.g. feature/my-branch"
              value={branch}
              onChange={(e) => setBranch(e.target.value)}
            />
            <div className="flex items-center justify-end gap-2">
              <Button size="sm" variant="ghost" onClick={onCancel}>
                <X size={14} />
                Cancel
              </Button>
              <Button size="sm" variant="primary" onClick={handleSubmit} disabled={!canSubmit}>
                <Play size={14} />
                Run
              </Button>
            </div>
          </div>
        )}
      </div>
    </>
  );
}
