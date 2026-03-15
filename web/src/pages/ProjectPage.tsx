import { FolderGit2, Link as LinkIcon, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { api, type Project } from "../lib/api";
import { Badge } from "../components/ui/Badge";
import { Button } from "../components/ui/Button";

type Tab = "sessions" | "loops" | "config";

export function ProjectPage() {
  const { projectId } = useParams<{ projectId: string }>();
  const navigate = useNavigate();
  const [project, setProject] = useState<Project | null>(null);
  const [loading, setLoading] = useState(true);
  const [activeTab, setActiveTab] = useState<Tab>("sessions");

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
          {(["sessions", "loops", "config"] as const).map((tab) => (
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
          <div className="text-sm text-text-tertiary">
            Sessions associated with this project will appear here.
          </div>
        )}
        {activeTab === "loops" && (
          <div className="text-sm text-text-tertiary">
            Agentic loops for this project will appear here.
          </div>
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
