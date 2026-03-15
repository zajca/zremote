import { ArrowLeft, Pencil, X } from "lucide-react";
import { useCallback, useRef, useState } from "react";
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
  const { sessions, refetch } = useSessions(hostId);
  const [closing, setClosing] = useState(false);
  const [editingName, setEditingName] = useState(false);
  const [nameValue, setNameValue] = useState("");
  const nameInputRef = useRef<HTMLInputElement>(null);

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

  const handleStartEditing = useCallback(() => {
    if (!session) return;
    setNameValue(session.name ?? "");
    setEditingName(true);
    setTimeout(() => nameInputRef.current?.focus(), 50);
  }, [session]);

  const handleRenameSubmit = useCallback(async () => {
    if (!sessionId) return;
    setEditingName(false);
    const trimmed = nameValue.trim();
    try {
      await api.sessions.rename(sessionId, trimmed || null);
      void refetch();
    } catch (e) {
      console.error("failed to rename session", e);
    }
  }, [sessionId, nameValue, refetch]);

  const handleRenameKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        void handleRenameSubmit();
      } else if (e.key === "Escape") {
        setEditingName(false);
      }
    },
    [handleRenameSubmit],
  );

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
        {editingName ? (
          <input
            ref={nameInputRef}
            value={nameValue}
            onChange={(e) => setNameValue(e.target.value)}
            onBlur={() => void handleRenameSubmit()}
            onKeyDown={handleRenameKeyDown}
            className="h-6 rounded border border-accent bg-bg-tertiary px-2 text-sm text-text-primary focus:ring-2 focus:ring-accent/20 focus:outline-none"
            placeholder="Session name"
          />
        ) : (
          <>
            <span className="text-sm text-text-primary">
              {session.name || session.shell || "shell"}
            </span>
            <button
              onClick={handleStartEditing}
              className="text-text-tertiary transition-colors duration-150 hover:text-text-primary"
              title="Rename session"
            >
              <Pencil size={12} />
            </button>
          </>
        )}
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
