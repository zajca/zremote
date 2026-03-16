import { useState } from "react";
import { api } from "../../lib/api";
import { useKnowledgeStore } from "../../stores/knowledge-store";
import { Button } from "../ui/Button";
import { Badge } from "../ui/Badge";
import { showToast } from "../layout/Toast";

function statusBadgeVariant(
  status: string,
): "online" | "offline" | "error" | "warning" | "creating" {
  switch (status) {
    case "ready":
      return "online";
    case "starting":
      return "warning";
    case "indexing":
      return "creating";
    case "error":
      return "error";
    case "stopped":
      return "offline";
    default:
      return "offline";
  }
}

export function KnowledgeStatus({
  projectId,
  hostId,
}: {
  projectId: string;
  hostId: string;
}) {
  const { statusByProject, controlService, triggerIndex, fetchStatus } =
    useKnowledgeStore();
  const status = statusByProject[projectId];
  const [bootstrapping, setBootstrapping] = useState(false);

  const handleBootstrap = async () => {
    setBootstrapping(true);
    try {
      await api.knowledge.bootstrapProject(projectId);
      showToast("Knowledge bootstrap started", "success");
    } catch (e) {
      console.error("Failed to bootstrap:", e);
      showToast("Failed to bootstrap knowledge", "error");
    } finally {
      setTimeout(() => {
        setBootstrapping(false);
        fetchStatus(projectId);
      }, 3000);
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Badge variant={statusBadgeVariant(status?.status ?? "stopped")}>
            {status?.status ?? "not configured"}
          </Badge>
          <span className="text-sm text-text-primary">OpenViking</span>
          {status?.openviking_version && (
            <span className="text-xs text-text-tertiary">
              v{status.openviking_version}
            </span>
          )}
        </div>
      </div>

      {status?.last_error && (
        <div className="rounded bg-status-error/10 p-2 text-xs text-status-error">
          {status.last_error}
        </div>
      )}

      {(!status || status.last_error?.includes("not enabled")) && (
        <div className="rounded border border-border-secondary bg-bg-secondary p-3 text-sm text-text-secondary">
          <p className="font-medium text-text-primary">OpenViking is not configured on this host.</p>
          <ol className="mt-2 list-inside list-decimal space-y-1">
            <li><code className="text-xs bg-bg-tertiary px-1 rounded">pip install openviking</code></li>
            <li>Set <code className="text-xs bg-bg-tertiary px-1 rounded">OPENVIKING_ENABLED=true</code></li>
            <li>Set <code className="text-xs bg-bg-tertiary px-1 rounded">OPENROUTER_API_KEY=sk-or-...</code></li>
            <li>Restart agent</li>
          </ol>
        </div>
      )}

      <div className="flex gap-2">
        {(!status ||
          status.status === "stopped" ||
          status.status === "error") && (
          <Button
            variant="primary"
            size="sm"
            onClick={async () => {
              try {
                await controlService(hostId, "start");
                showToast("Service starting", "success");
              } catch {
                showToast("Failed to start service", "error");
              }
              setTimeout(() => fetchStatus(projectId), 2000);
            }}
          >
            Start Service
          </Button>
        )}
        {status?.status === "ready" && (
          <>
            <Button
              variant="secondary"
              size="sm"
              onClick={async () => {
                try {
                  await controlService(hostId, "stop");
                  showToast("Service stopping", "success");
                } catch {
                  showToast("Failed to stop service", "error");
                }
                setTimeout(() => fetchStatus(projectId), 1000);
              }}
            >
              Stop
            </Button>
            <Button
              variant="secondary"
              size="sm"
              onClick={async () => {
                try {
                  await controlService(hostId, "restart");
                  showToast("Service restarting", "success");
                } catch {
                  showToast("Failed to restart service", "error");
                }
                setTimeout(() => fetchStatus(projectId), 3000);
              }}
            >
              Restart
            </Button>
            <Button
              variant="primary"
              size="sm"
              onClick={async () => {
                try {
                  await triggerIndex(projectId);
                  showToast("Indexing started", "success");
                } catch {
                  showToast("Failed to start indexing", "error");
                }
              }}
            >
              Index Project
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={async () => {
                try {
                  await triggerIndex(projectId, true);
                  showToast("Reindexing started", "success");
                } catch {
                  showToast("Failed to start reindexing", "error");
                }
              }}
            >
              Force Reindex
            </Button>
            <Button
              variant="secondary"
              size="sm"
              onClick={handleBootstrap}
              disabled={bootstrapping}
            >
              {bootstrapping ? "Bootstrapping..." : "Bootstrap Knowledge"}
            </Button>
          </>
        )}
      </div>
    </div>
  );
}
