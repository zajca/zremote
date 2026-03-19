import { useCallback } from "react";
import { BarChart3, Clock, ClipboardList, HelpCircle, Laptop, Monitor, PanelLeftClose, Pin, Settings } from "lucide-react";
import { Link } from "react-router";
import { useHosts } from "../../hooks/useHosts";
import { useMode } from "../../hooks/useMode";
import { useRealtimeUpdates } from "../../hooks/useRealtimeUpdates";
import { PROJECT_UPDATE_EVENT } from "../../hooks/useProjects";
import { SESSION_UPDATE_EVENT } from "../../hooks/useSessions";
import { HostItem } from "../sidebar/HostItem";
import { useCommandPaletteStore } from "../../stores/command-palette-store";

interface SidebarProps {
  pinned: boolean;
  onPin: () => void;
  onUnpin: () => void;
  onOpenHelp: () => void;
}

export function Sidebar({ pinned, onPin, onUnpin, onOpenHelp }: SidebarProps) {
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
          ZRemote
        </span>
        {isLocal && (
          <span className="ml-auto flex items-center gap-1 rounded bg-bg-tertiary px-1.5 py-0.5 text-[10px] text-text-tertiary">
            <Laptop size={10} />
            Local
          </span>
        )}
        <button
          onClick={pinned ? onUnpin : onPin}
          className={`${isLocal ? "" : "ml-auto "}inline-flex h-7 w-7 items-center justify-center rounded-md text-text-tertiary transition-colors hover:bg-bg-hover hover:text-text-primary`}
          title={pinned ? "Unpin sidebar (Ctrl+B)" : "Pin sidebar (Ctrl+B)"}
          aria-label={pinned ? "Unpin sidebar" : "Pin sidebar"}
        >
          {pinned ? <PanelLeftClose size={16} /> : <Pin size={16} />}
        </button>
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
        <button
          onClick={() => {
            const store = useCommandPaletteStore.getState();
            store.setOpen(true);
            store.pushContext({ level: "clipboard" });
          }}
          className="flex h-8 w-full items-center gap-2 rounded-md px-2 text-[13px] text-text-secondary transition-colors duration-150 hover:bg-bg-hover hover:text-text-primary"
          aria-label="Open clipboard history"
        >
          <ClipboardList size={16} />
          <span className="flex-1 text-left">Clipboard</span>
          <kbd className="rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px] text-text-tertiary">
            Alt+V
          </kbd>
        </button>
        <Link
          to="/settings"
          className="flex h-8 items-center gap-2 rounded-md px-2 text-[13px] text-text-secondary transition-colors duration-150 hover:bg-bg-hover hover:text-text-primary"
        >
          <Settings size={16} />
          Settings
        </Link>
        <button
          onClick={onOpenHelp}
          className="flex h-8 w-full items-center gap-2 rounded-md px-2 text-[13px] text-text-secondary transition-colors duration-150 hover:bg-bg-hover hover:text-text-primary"
          aria-label="Open help"
        >
          <HelpCircle size={16} />
          <span className="flex-1 text-left">Help</span>
          <kbd className="rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px] text-text-tertiary">?</kbd>
        </button>
      </div>
    </aside>
  );
}
