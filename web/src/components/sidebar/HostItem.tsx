import { ChevronRight, Plus } from "lucide-react";
import { memo, useCallback, useEffect, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import type { Host } from "../../lib/api";
import { useSessions } from "../../hooks/useSessions";
import { StatusDot } from "../ui/StatusDot";
import { SessionItem } from "./SessionItem";

interface HostItemProps {
  host: Host;
}

function getStorageKey(hostId: string) {
  return `myremote:host-expanded:${hostId}`;
}

export const HostItem = memo(function HostItem({ host }: HostItemProps) {
  const navigate = useNavigate();
  const location = useLocation();
  const isActive = location.pathname.startsWith(`/hosts/${host.id}`);

  const [expanded, setExpanded] = useState(() => {
    return localStorage.getItem(getStorageKey(host.id)) === "true";
  });

  const { sessions } = useSessions(expanded ? host.id : undefined);

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
        {sessions.length > 0 && (
          <span className="shrink-0 text-[11px] text-text-tertiary">
            {sessions.length}
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
          {sessions.map((session) => (
            <SessionItem
              key={session.id}
              session={session}
              hostId={host.id}
            />
          ))}
        </div>
      )}
    </div>
  );
});
