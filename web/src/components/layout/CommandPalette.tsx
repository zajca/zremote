import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { Command } from "cmdk";
import {
  BarChart3,
  Bot,
  Clock,
  FolderPlus,
  Laptop,
  Search,
  Settings,
} from "lucide-react";
import { useHosts } from "../../hooks/useHosts";
import { useMode } from "../../hooks/useMode";
import { useProjects } from "../../hooks/useProjects";
import { AddProjectDialog } from "../AddProjectDialog";
import { StartClaudeDialog } from "../StartClaudeDialog";
import type { Project } from "../../lib/api";

export function CommandPalette() {
  const [open, setOpen] = useState(false);
  const navigate = useNavigate();
  const { hosts } = useHosts();
  const { isLocal } = useMode();

  // Collect projects for all online hosts
  const onlineHost = hosts.find((h) => h.status === "online");
  const { projects } = useProjects(onlineHost?.id);

  const [addProjectHostId, setAddProjectHostId] = useState<string | null>(null);

  const [claudeDialogProject, setClaudeDialogProject] = useState<{
    project: Project;
    hostId: string;
  } | null>(null);

  // Toggle on Cmd+K / Ctrl+K
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setOpen((prev) => !prev);
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);

  const runAction = useCallback(
    (path: string) => {
      setOpen(false);
      void navigate(path);
    },
    [navigate],
  );

  const handleAddProject = useCallback(
    (hostId: string) => {
      setOpen(false);
      setAddProjectHostId(hostId);
    },
    [],
  );

  const handleStartClaude = useCallback(
    (project: Project, hostId: string) => {
      setOpen(false);
      setClaudeDialogProject({ project, hostId });
    },
    [],
  );

  if (!open && !claudeDialogProject && !addProjectHostId) return null;

  return (
    <>
      {open && (
        <div className="fixed inset-0 z-50 flex items-start justify-center pt-[20vh]">
          {/* Backdrop */}
          <div
            className="fixed inset-0 bg-black/50"
            onClick={() => setOpen(false)}
          />

          <Command
            className="relative w-full max-w-lg overflow-hidden rounded-xl border border-border bg-bg-secondary shadow-2xl"
            loop
          >
            <div className="flex items-center gap-2 border-b border-border px-3">
              <Search size={14} className="text-text-tertiary" />
              <Command.Input
                placeholder="Search commands..."
                className="h-10 w-full bg-transparent text-sm text-text-primary placeholder:text-text-tertiary focus:outline-none"
              />
            </div>

            <Command.List className="max-h-80 overflow-auto p-2">
              <Command.Empty className="px-3 py-6 text-center text-sm text-text-tertiary">
                No results found
              </Command.Empty>

              <Command.Group
                heading="Navigation"
                className="[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-text-tertiary"
              >
                <CommandItem
                  icon={<BarChart3 size={14} />}
                  onSelect={() => runAction("/analytics")}
                >
                  Open Analytics
                </CommandItem>
                <CommandItem
                  icon={<Clock size={14} />}
                  onSelect={() => runAction("/history")}
                >
                  Open History
                </CommandItem>
                <CommandItem
                  icon={<Settings size={14} />}
                  onSelect={() => runAction("/settings")}
                >
                  Open Settings
                </CommandItem>
              </Command.Group>

              {!isLocal && hosts.length > 0 && (
                <Command.Group
                  heading="Hosts"
                  className="[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-text-tertiary"
                >
                  {hosts.map((host) => (
                    <CommandItem
                      key={host.id}
                      icon={<Laptop size={14} />}
                      onSelect={() => runAction(`/hosts/${host.id}`)}
                    >
                      Go to {host.hostname}
                    </CommandItem>
                  ))}
                </Command.Group>
              )}

              {onlineHost && (
                <Command.Group
                  heading="Projects"
                  className="[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-text-tertiary"
                >
                  <CommandItem
                    icon={<FolderPlus size={14} />}
                    onSelect={() => handleAddProject(onlineHost.id)}
                  >
                    Add project
                  </CommandItem>
                </Command.Group>
              )}

              {onlineHost && projects.length > 0 && (
                <Command.Group
                  heading="Start Claude"
                  className="[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-text-tertiary"
                >
                  {projects.map((project) => (
                    <CommandItem
                      key={project.id}
                      icon={<Bot size={14} className="text-accent" />}
                      onSelect={() =>
                        handleStartClaude(project, onlineHost.id)
                      }
                    >
                      Start Claude on {project.name}
                    </CommandItem>
                  ))}
                </Command.Group>
              )}
            </Command.List>

            <div className="flex items-center justify-between border-t border-border px-3 py-1.5 text-xs text-text-tertiary">
              <span>Navigate with arrow keys</span>
              <div className="flex items-center gap-2">
                <kbd className="rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px]">
                  Esc
                </kbd>
                <span>to close</span>
              </div>
            </div>
          </Command>
        </div>
      )}

      {addProjectHostId && (
        <AddProjectDialog
          hostId={addProjectHostId}
          open={true}
          onClose={() => setAddProjectHostId(null)}
        />
      )}

      {claudeDialogProject && (
        <StartClaudeDialog
          projectName={claudeDialogProject.project.name}
          projectPath={claudeDialogProject.project.path}
          hostId={claudeDialogProject.hostId}
          projectId={claudeDialogProject.project.id}
          onClose={() => setClaudeDialogProject(null)}
        />
      )}
    </>
  );
}

function CommandItem({
  children,
  icon,
  onSelect,
}: {
  children: React.ReactNode;
  icon: React.ReactNode;
  onSelect: () => void;
}) {
  return (
    <Command.Item
      onSelect={onSelect}
      className="flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-sm text-text-secondary transition-colors duration-75 data-[selected=true]:bg-bg-hover data-[selected=true]:text-text-primary"
    >
      {icon}
      {children}
    </Command.Item>
  );
}
