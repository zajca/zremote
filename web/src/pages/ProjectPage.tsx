import { FolderGit2, Link as LinkIcon, Terminal, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { api, type Project, type Session } from "../lib/api";
import { Badge } from "../components/ui/Badge";
import { Button } from "../components/ui/Button";
import { SessionItem } from "../components/sidebar/SessionItem";
import { KnowledgePanel } from "../components/knowledge/KnowledgePanel";

type Tab = "sessions" | "loops" | "knowledge" | "config";

export function ProjectPage() {
  const { projectId } = useParams<{ projectId: string }>();
  const navigate = useNavigate();
  const [project, setProject] = useState<Project | null>(null);
  const [loading, setLoading] = useState(true);
  const [activeTab, setActiveTab] = useState<Tab>("sessions");
  const [projectSessions, setProjectSessions] = useState<Session[]>([]);

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
      const session = await api.sessions.create(
        project.host_id,
        80,
        24,
        project.path,
      );
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
          {(["sessions", "loops", "knowledge", "config"] as const).map((tab) => (
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
      </div>
    </div>
  );
}
