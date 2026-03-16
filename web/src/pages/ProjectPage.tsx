import {
  FolderGit2,
  GitBranch,
  Link as LinkIcon,
  Plus,
  RefreshCw,
  Terminal,
  Trash2,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { api, type Project, type Session } from "../lib/api";
import { Badge } from "../components/ui/Badge";
import { Button } from "../components/ui/Button";
import { Input } from "../components/ui/Input";
import { SessionItem } from "../components/sidebar/SessionItem";
import { KnowledgePanel } from "../components/knowledge/KnowledgePanel";

type Tab = "sessions" | "loops" | "knowledge" | "config" | "git";

export function ProjectPage() {
  const { projectId } = useParams<{ projectId: string }>();
  const navigate = useNavigate();
  const [project, setProject] = useState<Project | null>(null);
  const [loading, setLoading] = useState(true);
  const [activeTab, setActiveTab] = useState<Tab>("sessions");
  const [projectSessions, setProjectSessions] = useState<Session[]>([]);
  const [worktrees, setWorktrees] = useState<Project[]>([]);
  const [refreshing, setRefreshing] = useState(false);
  const [showCreateWorktree, setShowCreateWorktree] = useState(false);
  const [newBranch, setNewBranch] = useState("");
  const [newPath, setNewPath] = useState("");
  const [isNewBranch, setIsNewBranch] = useState(false);
  const [creating, setCreating] = useState(false);

  useEffect(() => {
    if (!projectId) return;
    setLoading(true);
    void api.projects.get(projectId).then(
      (p) => {
        setProject(p);
        setLoading(false);
      },
      () => {
        setProject(null);
        setLoading(false);
      },
    );
  }, [projectId]);

  useEffect(() => {
    if (!projectId) return;
    void api.projects.sessions(projectId).then(
      (s) => setProjectSessions(s),
      () => setProjectSessions([]),
    );
  }, [projectId]);

  const loadWorktrees = useCallback(() => {
    if (!projectId) return;
    void api.projects.worktrees(projectId).then(
      (wt) => setWorktrees(wt),
      () => setWorktrees([]),
    );
  }, [projectId]);

  useEffect(() => {
    if (activeTab === "git" && projectId) {
      loadWorktrees();
    }
  }, [activeTab, projectId, loadWorktrees]);

  const handleRefreshGit = useCallback(async () => {
    if (!projectId) return;
    setRefreshing(true);
    try {
      await api.projects.refreshGit(projectId);
      const p = await api.projects.get(projectId);
      setProject(p);
      loadWorktrees();
    } catch (e) {
      console.error("failed to refresh git", e);
    } finally {
      setRefreshing(false);
    }
  }, [projectId, loadWorktrees]);

  const handleCreateWorktree = useCallback(async () => {
    if (!projectId || !newBranch.trim()) return;
    setCreating(true);
    try {
      await api.projects.createWorktree(projectId, {
        branch: newBranch.trim(),
        path: newPath.trim() || undefined,
        new_branch: isNewBranch || undefined,
      });
      setNewBranch("");
      setNewPath("");
      setIsNewBranch(false);
      setShowCreateWorktree(false);
      loadWorktrees();
    } catch (e) {
      console.error("failed to create worktree", e);
    } finally {
      setCreating(false);
    }
  }, [projectId, newBranch, newPath, isNewBranch, loadWorktrees]);

  const handleDeleteWorktree = useCallback(
    async (worktreeId: string, worktreeName: string) => {
      if (!projectId) return;
      if (!window.confirm(`Delete worktree "${worktreeName}"?`)) return;
      try {
        await api.projects.deleteWorktree(projectId, worktreeId);
        loadWorktrees();
      } catch (e) {
        console.error("failed to delete worktree", e);
      }
    },
    [projectId, loadWorktrees],
  );

  const handleOpenWorktreeTerminal = useCallback(
    async (wt: Project) => {
      if (!project) return;
      try {
        const session = await api.sessions.create(project.host_id, {
          workingDir: wt.path,
        });
        void navigate(`/hosts/${project.host_id}/sessions/${session.id}`);
      } catch (e) {
        console.error("failed to create terminal session", e);
      }
    },
    [project, navigate],
  );

  const parsedRemotes = useMemo(() => {
    if (!project?.git_remotes) return [];
    try {
      return JSON.parse(project.git_remotes) as { name: string; url: string }[];
    } catch {
      return [];
    }
  }, [project?.git_remotes]);

  const handleDelete = useCallback(async () => {
    if (!projectId || !project) return;
    if (!window.confirm(`Remove project "${project.name}" from tracking?`))
      return;
    try {
      await api.projects.delete(projectId);
      void navigate("/");
    } catch (e) {
      console.error("failed to delete project", e);
    }
  }, [projectId, project, navigate]);

  const handleOpenTerminal = useCallback(async () => {
    if (!project) return;
    try {
      const session = await api.sessions.create(project.host_id, {
        workingDir: project.path,
      });
      void navigate(`/hosts/${project.host_id}/sessions/${session.id}`);
    } catch (e) {
      console.error("failed to create terminal session", e);
    }
  }, [project, navigate]);

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center text-text-secondary">
        Loading project...
      </div>
    );
  }

  if (!project) {
    return (
      <div className="flex h-full items-center justify-center text-text-secondary">
        Project not found
      </div>
    );
  }

  const typeColor =
    project.project_type === "rust"
      ? "online"
      : project.project_type === "node"
        ? "creating"
        : project.project_type === "python"
          ? "offline"
          : "offline";

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between border-b border-border px-6 py-4">
        <div className="flex items-center gap-3">
          <FolderGit2 size={20} className="text-accent" />
          <h1 className="text-lg font-semibold text-text-primary">
            {project.name}
          </h1>
          <Badge variant={typeColor}>{project.project_type}</Badge>
          {project.has_claude_config && (
            <Badge variant="online">.claude</Badge>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Button
            onClick={() =>
              void navigate(`/hosts/${project.host_id}`)
            }
            variant="ghost"
            size="sm"
          >
            <LinkIcon size={14} />
            Host
          </Button>
          <Button
            onClick={() => void handleOpenTerminal()}
            variant="ghost"
            size="sm"
          >
            <Terminal size={14} />
            Terminal
          </Button>
          <Button
            onClick={() => void handleDelete()}
            variant="ghost"
            size="sm"
          >
            <Trash2 size={14} />
            Remove
          </Button>
        </div>
      </div>

      <div className="border-b border-border px-6">
        <div className="flex items-center gap-1 text-sm text-text-tertiary">
          <span className="font-mono">{project.path}</span>
        </div>
        <div className="mt-2 flex gap-4">
          {(
            [
              "sessions",
              "loops",
              ...(project.git_branch !== null ? ["git" as const] : []),
              "knowledge",
              "config",
            ] as const
          ).map((tab) => (
            <button
              key={tab}
              onClick={() => setActiveTab(tab)}
              className={`border-b-2 px-1 pb-2 text-sm capitalize transition-colors duration-150 ${
                activeTab === tab
                  ? "border-accent text-text-primary"
                  : "border-transparent text-text-tertiary hover:text-text-secondary"
              }`}
            >
              {tab}
            </button>
          ))}
        </div>
      </div>

      <div className="flex-1 overflow-auto p-6">
        {activeTab === "sessions" && (
          <div>
            {projectSessions.length === 0 ? (
              <div className="text-sm text-text-tertiary">
                No sessions linked to this project yet.
              </div>
            ) : (
              <div className="space-y-1">
                {projectSessions.map((session) => (
                  <SessionItem
                    key={session.id}
                    session={session}
                    hostId={project.host_id}
                  />
                ))}
              </div>
            )}
          </div>
        )}
        {activeTab === "loops" && (
          <div className="text-sm text-text-tertiary">
            Agentic loops for this project will appear here.
          </div>
        )}
        {activeTab === "knowledge" && (
          <KnowledgePanel projectId={project.id} hostId={project.host_id} />
        )}
        {activeTab === "config" && (
          <div className="text-sm text-text-tertiary">
            Project configuration (.claude/) will appear here.
          </div>
        )}
        {activeTab === "git" && (
          <div className="space-y-6">
            {/* Status section */}
            <div>
              <div className="mb-3 flex items-center justify-between">
                <h2 className="text-sm font-medium text-text-primary">
                  Status
                </h2>
                <Button
                  onClick={() => void handleRefreshGit()}
                  variant="ghost"
                  size="sm"
                  disabled={refreshing}
                >
                  <RefreshCw
                    size={14}
                    className={refreshing ? "animate-spin" : ""}
                  />
                  Refresh
                </Button>
              </div>
              <div className="rounded-md border border-border bg-bg-secondary p-4">
                <div className="grid grid-cols-2 gap-4 text-sm">
                  <div>
                    <span className="text-text-tertiary">Branch</span>
                    <div className="mt-0.5 flex items-center gap-1.5 text-text-primary">
                      <GitBranch size={14} />
                      {project.git_branch ?? "N/A"}
                    </div>
                  </div>
                  <div>
                    <span className="text-text-tertiary">Commit</span>
                    <div className="mt-0.5 font-mono text-xs text-text-primary">
                      {project.git_commit_hash
                        ? project.git_commit_hash.slice(0, 12)
                        : "N/A"}
                    </div>
                  </div>
                  <div className="col-span-2">
                    <span className="text-text-tertiary">Message</span>
                    <div className="mt-0.5 text-text-primary">
                      {project.git_commit_message ?? "N/A"}
                    </div>
                  </div>
                  <div>
                    <span className="text-text-tertiary">Working tree</span>
                    <div className="mt-0.5">
                      {project.git_is_dirty ? (
                        <Badge variant="warning">dirty</Badge>
                      ) : (
                        <Badge variant="online">clean</Badge>
                      )}
                    </div>
                  </div>
                  <div>
                    <span className="text-text-tertiary">Ahead / Behind</span>
                    <div className="mt-0.5 text-text-primary">
                      +{project.git_ahead} / -{project.git_behind}
                    </div>
                  </div>
                </div>
              </div>
            </div>

            {/* Remotes */}
            {parsedRemotes.length > 0 && (
              <div>
                <h2 className="mb-3 text-sm font-medium text-text-primary">
                  Remotes
                </h2>
                <div className="overflow-hidden rounded-md border border-border">
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="border-b border-border bg-bg-secondary">
                        <th className="px-4 py-2 text-left font-medium text-text-tertiary">
                          Name
                        </th>
                        <th className="px-4 py-2 text-left font-medium text-text-tertiary">
                          URL
                        </th>
                      </tr>
                    </thead>
                    <tbody>
                      {parsedRemotes.map((remote) => (
                        <tr
                          key={remote.name}
                          className="border-b border-border last:border-0"
                        >
                          <td className="px-4 py-2 font-mono text-text-primary">
                            {remote.name}
                          </td>
                          <td className="px-4 py-2 font-mono text-xs text-text-secondary">
                            {remote.url}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            )}

            {/* Worktrees */}
            <div>
              <div className="mb-3 flex items-center justify-between">
                <h2 className="text-sm font-medium text-text-primary">
                  Worktrees
                </h2>
                <Button
                  onClick={() => setShowCreateWorktree(true)}
                  variant="ghost"
                  size="sm"
                >
                  <Plus size={14} />
                  Create Worktree
                </Button>
              </div>

              {showCreateWorktree && (
                <div className="mb-4 rounded-md border border-border bg-bg-secondary p-4">
                  <div className="mb-3 flex items-center justify-between">
                    <span className="text-sm font-medium text-text-primary">
                      Create Worktree
                    </span>
                    <button
                      onClick={() => setShowCreateWorktree(false)}
                      className="text-text-tertiary hover:text-text-primary"
                    >
                      <X size={14} />
                    </button>
                  </div>
                  <div className="space-y-3">
                    <Input
                      label="Branch name"
                      value={newBranch}
                      onChange={(e) => setNewBranch(e.target.value)}
                      placeholder="feature/my-branch"
                    />
                    <Input
                      label="Path (optional)"
                      value={newPath}
                      onChange={(e) => setNewPath(e.target.value)}
                      placeholder="Leave empty for default location"
                    />
                    <label className="flex items-center gap-2 text-sm text-text-secondary">
                      <input
                        type="checkbox"
                        checked={isNewBranch}
                        onChange={(e) => setIsNewBranch(e.target.checked)}
                        className="rounded border-border"
                      />
                      Create new branch
                    </label>
                    <Button
                      onClick={() => void handleCreateWorktree()}
                      disabled={!newBranch.trim() || creating}
                      size="sm"
                    >
                      {creating ? "Creating..." : "Create"}
                    </Button>
                  </div>
                </div>
              )}

              {worktrees.length === 0 ? (
                <div className="text-sm text-text-tertiary">
                  No worktrees found.
                </div>
              ) : (
                <div className="space-y-2">
                  {worktrees.map((wt) => (
                    <div
                      key={wt.id}
                      className="flex items-center justify-between rounded-md border border-border bg-bg-secondary px-4 py-3"
                    >
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-2">
                          <GitBranch
                            size={14}
                            className="shrink-0 text-text-tertiary"
                          />
                          <span className="truncate text-sm font-medium text-text-primary">
                            {wt.name}
                          </span>
                          {wt.git_branch && (
                            <span className="shrink-0 rounded bg-bg-active px-1.5 py-0.5 text-xs text-text-tertiary">
                              {wt.git_branch}
                            </span>
                          )}
                          {wt.git_is_dirty && (
                            <span
                              className="h-1.5 w-1.5 shrink-0 rounded-full bg-status-warning"
                              title="Uncommitted changes"
                            />
                          )}
                        </div>
                        <div className="mt-1 truncate font-mono text-xs text-text-tertiary">
                          {wt.path}
                        </div>
                      </div>
                      <div className="ml-4 flex shrink-0 items-center gap-1">
                        <Button
                          onClick={() => void handleOpenWorktreeTerminal(wt)}
                          variant="ghost"
                          size="sm"
                        >
                          <Terminal size={14} />
                        </Button>
                        <Button
                          onClick={() =>
                            void handleDeleteWorktree(wt.id, wt.name)
                          }
                          variant="ghost"
                          size="sm"
                        >
                          <Trash2 size={14} />
                        </Button>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
