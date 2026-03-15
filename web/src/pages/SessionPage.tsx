import { ArrowLeft, X } from "lucide-react";
import { useCallback, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { useHosts } from "../hooks/useHosts";
import { useSessions } from "../hooks/useSessions";
import { api } from "../lib/api";
import { Badge } from "../components/ui/Badge";
import { IconButton } from "../components/ui/IconButton";
import { Terminal } from "../components/Terminal";

export function SessionPage() {
  const { hostId, sessionId } = useParams<{
    hostId: string;
    sessionId: string;
  }>();
  const navigate = useNavigate();
  const { hosts } = useHosts();
  const { sessions } = useSessions(hostId);
  const [closing, setClosing] = useState(false);

  const host = hosts.find((h) => h.id === hostId);
  const session = sessions.find((s) => s.id === sessionId);

  const handleClose = useCallback(async () => {
    if (!hostId || !sessionId || closing) return;
    if (!window.confirm("Close this session?")) return;
    setClosing(true);
    try {
      await api.sessions.close(hostId, sessionId);
      void navigate(`/hosts/${hostId}`);
    } catch {
      setClosing(false);
    }
  }, [hostId, sessionId, closing, navigate]);

  if (!session) {
    return (
      <div className="flex h-full items-center justify-center text-text-secondary">
        Session not found
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-3 border-b border-border px-4 py-2">
        <Link
          to={`/hosts/${hostId}`}
          className="text-text-tertiary transition-colors duration-150 hover:text-text-primary"
        >
          <ArrowLeft size={16} />
        </Link>
        <span className="font-mono text-xs text-text-tertiary">
          {sessionId?.slice(0, 8)}
        </span>
        <span className="text-sm text-text-primary">{session.shell ?? "shell"}</span>
        {host && (
          <Link
            to={`/hosts/${hostId}`}
            className="text-xs text-text-tertiary transition-colors duration-150 hover:text-accent"
          >
            {host.hostname}
          </Link>
        )}
        <Badge
          variant={
            session.status === "active"
              ? "online"
              : session.status === "error"
                ? "error"
                : session.status === "creating"
                  ? "creating"
                  : "offline"
          }
        >
          {session.status}
        </Badge>
        <div className="ml-auto">
          <IconButton
            icon={X}
            tooltip="Close session"
            onClick={() => void handleClose()}
            disabled={closing || session.status === "closed"}
          />
        </div>
      </div>

      <div className="min-h-0 flex-1">
        {sessionId && <Terminal sessionId={sessionId} />}
      </div>
    </div>
  );
}
