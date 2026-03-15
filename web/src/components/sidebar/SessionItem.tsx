import { Terminal } from "lucide-react";
import { memo, useCallback } from "react";
import { useLocation, useNavigate } from "react-router";
import type { Session } from "../../lib/api";
import { Badge } from "../ui/Badge";

interface SessionItemProps {
  session: Session;
  hostId: string;
}

function sessionStatusVariant(
  status: Session["status"],
): "online" | "offline" | "error" | "warning" | "creating" {
  switch (status) {
    case "active":
      return "online";
    case "closed":
      return "offline";
    case "error":
      return "error";
    case "creating":
      return "creating";
    default:
      return "offline";
  }
}

export const SessionItem = memo(function SessionItem({
  session,
  hostId,
}: SessionItemProps) {
  const navigate = useNavigate();
  const location = useLocation();
  const isActive = location.pathname.includes(`/sessions/${session.id}`);

  const handleClick = useCallback(() => {
    void navigate(`/hosts/${hostId}/sessions/${session.id}`);
  }, [navigate, hostId, session.id]);

  return (
    <button
      onClick={handleClick}
      className={`flex h-7 w-full items-center gap-2 px-2 text-[13px] transition-colors duration-150 hover:bg-bg-hover ${isActive ? "bg-bg-hover text-text-primary" : "text-text-secondary"}`}
    >
      <Terminal size={13} className="shrink-0 text-text-tertiary" />
      <span className="truncate">{session.shell ?? "shell"}</span>
      <Badge variant={sessionStatusVariant(session.status)}>
        {session.status}
      </Badge>
    </button>
  );
});
