import { Plus, Terminal } from "lucide-react";
import { useCallback, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { useHosts } from "../hooks/useHosts";
import { useSessions } from "../hooks/useSessions";
import { api } from "../lib/api";
import { Badge } from "../components/ui/Badge";
import { Button } from "../components/ui/Button";
import { StatusDot } from "../components/ui/StatusDot";
import { NewSessionDialog } from "../components/NewSessionDialog";

export function HostPage() {
  const { hostId } = useParams<{ hostId: string }>();
  const navigate = useNavigate();
  const { hosts } = useHosts();
  const { sessions, loading } = useSessions(hostId);
  const [showNewSession, setShowNewSession] = useState(false);

  const host = hosts.find((h) => h.id === hostId);

  const handleNewSessionSubmit = useCallback(
    async (options: { name?: string; shell?: string; workingDir?: string }) => {
      if (!hostId) return;
      setShowNewSession(false);
      try {
        const session = await api.sessions.create(hostId, options);
        void navigate(`/hosts/${hostId}/sessions/${session.id}`);
      } catch (e) {
        console.error("failed to create session", e);
        alert("Failed to create session. Check the console for details.");
      }
    },
    [hostId, navigate],
  );

  if (!host) {
    return (
      <div className="flex h-full items-center justify-center text-text-secondary">
        Host not found
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between border-b border-border px-6 py-4">
        <div className="flex items-center gap-3">
          <StatusDot
            status={host.status === "online" ? "online" : "offline"}
            pulse={host.status === "online"}
          />
          <h1 className="text-lg font-semibold text-text-primary">
            {host.hostname}
          </h1>
          <span className="text-sm text-text-tertiary">
            {host.os}/{host.arch}
          </span>
          <span className="text-sm text-text-tertiary">
            v{host.agent_version}
          </span>
          <span className="text-xs text-text-tertiary">
            Last seen: {new Date(host.last_seen).toLocaleString()}
          </span>
        </div>
        <Button onClick={() => setShowNewSession(true)} size="sm">
          <Plus size={14} />
          New Session
        </Button>
      </div>

      <div className="flex-1 overflow-auto p-6">
        {loading ? (
          <div className="text-sm text-text-tertiary">Loading sessions...</div>
        ) : sessions.length === 0 ? (
          <div className="flex flex-col items-center gap-4 pt-24 text-center">
            <Terminal size={32} className="text-text-tertiary" />
            <p className="text-sm text-text-secondary">No active sessions</p>
            <Button onClick={() => setShowNewSession(true)} size="sm">
              <Plus size={14} />
              Start Session
            </Button>
          </div>
        ) : (
          <div className="space-y-1">
            {sessions.map((session) => (
              <button
                key={session.id}
                onClick={() =>
                  void navigate(
                    `/hosts/${hostId}/sessions/${session.id}`,
                  )
                }
                className="flex w-full items-center gap-4 rounded-md px-3 py-2 text-left transition-colors duration-150 hover:bg-bg-hover"
              >
                <Terminal size={16} className="shrink-0 text-text-tertiary" />
                <span className="text-sm text-text-primary">
                  {session.name || session.shell || "shell"}
                </span>
                <span className="font-mono text-xs text-text-tertiary">
                  {session.id.slice(0, 8)}
                </span>
                {session.working_dir && (
                  <span className="truncate font-mono text-xs text-text-tertiary">
                    {session.working_dir.split("/").pop()}
                  </span>
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
                <span className="ml-auto text-xs text-text-tertiary">
                  {new Date(session.created_at).toLocaleString()}
                </span>
              </button>
            ))}
          </div>
        )}
      </div>

      <NewSessionDialog
        open={showNewSession}
        onClose={() => setShowNewSession(false)}
        onSubmit={(options) => void handleNewSessionSubmit(options)}
      />
    </div>
  );
}
