import { ChevronRight, Plus, Search } from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import type { Host } from "../../lib/api";
import { api } from "../../lib/api";
import { useProjects } from "../../hooks/useProjects";
import { useSessions } from "../../hooks/useSessions";
import { StatusDot } from "../ui/StatusDot";
import { ProjectItem } from "./ProjectItem";
import { SessionItem } from "./SessionItem";
import { showToast } from "../layout/Toast";

interface HostItemProps {
  host: Host;
}

function getStorageKey(hostId: string) {
  return `zremote:host-expanded:${hostId}`;
}

export const HostItem = memo(function HostItem({ host }: HostItemProps) {
  const navigate = useNavigate();
  const location = useLocation();
  const isActive = location.pathname.startsWith(`/hosts/${host.id}`);

  const [expanded, setExpanded] = useState(() => {
    return localStorage.getItem(getStorageKey(host.id)) === "true";
  });

  const { sessions } = useSessions(expanded ? host.id : undefined);
  const { projects } = useProjects(expanded ? host.id : undefined);

  const handleScanProjects = useCallback(
    async (e: React.MouseEvent) => {
      e.stopPropagation();
      try {
        await api.projects.scan(host.id);
        showToast("Project scan started", "success");
      } catch {
        showToast("Failed to scan projects", "error");
      }
    },
    [host.id],
  );

  useEffect(() => {
    localStorage.setItem(getStorageKey(host.id), String(expanded));
  }, [expanded, host.id]);

  const toggle = useCallback(() => {
    setExpanded((prev) => !prev);
  }, []);

  const handleHostClick = useCallback(() => {
    void navigate(`/hosts/${host.id}`);
  }, [navigate, host.id]);

  const handleNewSession = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      void navigate(`/hosts/${host.id}`);
    },
    [navigate, host.id],
  );

  // Split sessions into project-linked and orphan
  const { projectSessionsMap, orphanSessions, activeSessions } = useMemo(() => {
    const map = new Map<string, typeof sessions>();
    const orphans: typeof sessions = [];
    const nonClosed = sessions.filter((s) => s.status !== "closed");
    for (const session of nonClosed) {
      if (session.project_id) {
        const existing = map.get(session.project_id) ?? [];
        existing.push(session);
        map.set(session.project_id, existing);
      } else {
        orphans.push(session);
      }
    }
    return { projectSessionsMap: map, orphanSessions: orphans, activeSessions: nonClosed };
  }, [sessions]);

  // Separate root projects from worktree children
  const rootProjects = useMemo(
    () => projects.filter((p) => p.parent_project_id === null),
    [projects],
  );

  return (
    <div>
      <div
        className={`group flex h-8 cursor-pointer items-center gap-1.5 px-2 transition-colors duration-150 hover:bg-bg-hover ${isActive ? "bg-bg-hover text-text-primary" : "text-text-secondary"}`}
      >
        <button
          onClick={toggle}
          className="flex h-5 w-5 shrink-0 items-center justify-center rounded transition-colors duration-150 hover:bg-bg-active"
          aria-label={expanded ? "Collapse" : "Expand"}
        >
          <ChevronRight
            size={14}
            className={`transition-transform duration-150 ${expanded ? "rotate-90" : ""}`}
          />
        </button>
        <StatusDot
          status={host.status === "online" ? "online" : "offline"}
          pulse={host.status === "online"}
        />
        <button
          onClick={handleHostClick}
          className="flex min-w-0 flex-1 items-center gap-2 truncate text-left text-[13px]"
        >
          <span className="truncate">{host.hostname}</span>
        </button>
        {activeSessions.length > 0 && (
          <span className="shrink-0 text-[11px] text-text-tertiary">
            {activeSessions.length}
          </span>
        )}
        <button
          onClick={handleNewSession}
          className="hidden h-5 w-5 shrink-0 items-center justify-center rounded text-text-tertiary transition-colors duration-150 hover:bg-bg-active hover:text-text-primary group-hover:flex"
          aria-label="New session"
        >
          <Plus size={14} />
        </button>
      </div>
      {expanded && (
        <div className="ml-4">
          {rootProjects.length > 0 && (
            <div className="mb-0.5">
              <div className="flex items-center justify-between px-2 py-0.5">
                <span className="text-[10px] font-medium tracking-wider text-text-tertiary uppercase">
                  Projects
                </span>
                <button
                  onClick={handleScanProjects}
                  className="flex h-4 w-4 items-center justify-center rounded text-text-tertiary hover:bg-bg-active hover:text-text-primary"
                  title="Scan for projects"
                >
                  <Search size={10} />
                </button>
              </div>
              {rootProjects.map((project) => {
                const worktreeChildren = projects.filter(
                  (p) => p.parent_project_id === project.id,
                );
                return (
                  <ProjectItem
                    key={project.id}
                    project={project}
                    sessions={projectSessionsMap.get(project.id) ?? []}
                    hostId={host.id}
                    worktreeChildren={worktreeChildren}
                    projectSessionsMap={projectSessionsMap}
                  />
                );
              })}
            </div>
          )}
          {orphanSessions.length > 0 && (
            <div>
              {projects.length > 0 && (
                <div className="px-2 py-0.5">
                  <span className="text-[10px] font-medium tracking-wider text-text-tertiary uppercase">
                    Sessions
                  </span>
                </div>
              )}
              {orphanSessions.map((session) => (
                <SessionItem
                  key={session.id}
                  session={session}
                  hostId={host.id}
                />
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
});
