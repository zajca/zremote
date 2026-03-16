import { useCallback } from "react";
import { BarChart3, Clock, Laptop, Monitor, Settings } from "lucide-react";
import { Link } from "react-router";
import { useHosts } from "../../hooks/useHosts";
import { useMode } from "../../hooks/useMode";
import { useRealtimeUpdates } from "../../hooks/useRealtimeUpdates";
import { PROJECT_UPDATE_EVENT } from "../../hooks/useProjects";
import { SESSION_UPDATE_EVENT } from "../../hooks/useSessions";
import { HostItem } from "../sidebar/HostItem";

export function Sidebar() {
  const { hosts, loading, refetch: refetchHosts } = useHosts();
  const { isLocal } = useMode();

  const onSessionUpdate = useCallback(() => {
    window.dispatchEvent(new Event(SESSION_UPDATE_EVENT));
  }, []);

  const onProjectUpdate = useCallback(() => {
    window.dispatchEvent(new Event(PROJECT_UPDATE_EVENT));
  }, []);

  useRealtimeUpdates({
    onHostUpdate: refetchHosts,
    onSessionUpdate,
    onProjectUpdate,
  });

  return (
    <aside className="flex h-full w-64 shrink-0 flex-col border-r border-border bg-bg-secondary">
      <div className="flex h-12 items-center gap-2 border-b border-border px-4">
        <Monitor size={18} className="text-accent" />
        <span className="text-sm font-semibold text-text-primary">
          MyRemote
        </span>
        {isLocal && (
          <span className="ml-auto flex items-center gap-1 rounded bg-bg-tertiary px-1.5 py-0.5 text-[10px] text-text-tertiary">
            <Laptop size={10} />
            Local
          </span>
        )}
      </div>

      <nav className="sidebar-scroll flex-1 overflow-y-auto py-2">
        {loading ? (
          <div className="px-4 py-2 text-[13px] text-text-tertiary">
            Loading hosts...
          </div>
        ) : hosts.length === 0 ? (
          <div className="px-4 py-2 text-[13px] text-text-tertiary">
            {isLocal ? "Waiting for local agent..." : "No hosts connected"}
          </div>
        ) : (
          hosts.map((host) => <HostItem key={host.id} host={host} />)
        )}
      </nav>

      <div className="space-y-0.5 border-t border-border p-2">
        <Link
          to="/analytics"
          className="flex h-8 items-center gap-2 rounded-md px-2 text-[13px] text-text-secondary transition-colors duration-150 hover:bg-bg-hover hover:text-text-primary"
        >
          <BarChart3 size={16} />
          Analytics
        </Link>
        <Link
          to="/history"
          className="flex h-8 items-center gap-2 rounded-md px-2 text-[13px] text-text-secondary transition-colors duration-150 hover:bg-bg-hover hover:text-text-primary"
        >
          <Clock size={16} />
          History
        </Link>
        <Link
          to="/settings"
          className="flex h-8 items-center gap-2 rounded-md px-2 text-[13px] text-text-secondary transition-colors duration-150 hover:bg-bg-hover hover:text-text-primary"
        >
          <Settings size={16} />
          Settings
        </Link>
      </div>
    </aside>
  );
}
