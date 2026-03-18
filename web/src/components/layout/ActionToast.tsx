import { useMemo } from "react";
import { useNavigate } from "react-router";
import { AlertCircle, Check, X, Terminal } from "lucide-react";
import { useNotificationStore } from "../../stores/notification-store";
import { useAgenticStore } from "../../stores/agentic-store";
import { showToast } from "./Toast";
import type { ActionNotification } from "../../stores/notification-store";

const MAX_VISIBLE = 3;

function ActionToastItem({ notification }: { notification: ActionNotification }) {
  const navigate = useNavigate();
  const dismiss = useNotificationStore((s) => s.dismiss);

  const title =
    notification.pendingToolCount > 1
      ? `${String(notification.pendingToolCount)} tool calls pending`
      : notification.status === "tool_pending" && notification.latestToolName
        ? notification.latestToolName
        : notification.toolName || "Claude needs input";

  const handleApprove = async () => {
    try {
      await useAgenticStore.getState().sendAction(notification.loopId, "approve");
      dismiss(notification.loopId);
    } catch {
      showToast("Failed to approve", "error");
    }
  };

  const handleReject = async () => {
    try {
      await useAgenticStore.getState().sendAction(notification.loopId, "reject");
      dismiss(notification.loopId);
    } catch {
      showToast("Failed to reject", "error");
    }
  };

  const handleNavigate = () => {
    void navigate(`/hosts/${notification.hostId}/sessions/${notification.sessionId}`);
    dismiss(notification.loopId);
  };

  return (
    <div
      role="alert"
      className="relative flex min-w-[300px] max-w-[400px] gap-3 rounded-lg border-l-4 border-l-status-warning bg-bg-secondary p-3 shadow-lg"
    >
      <AlertCircle
        size={18}
        className="mt-0.5 shrink-0 animate-pulse text-status-warning"
      />

      <div className="flex min-w-0 flex-1 flex-col gap-2">
        <div>
          <div className="text-sm font-medium text-text-primary">{title}</div>
          {notification.argumentsPreview && (
            <div className="truncate font-mono text-xs text-text-tertiary">
              {notification.argumentsPreview}
            </div>
          )}
          {notification.hostname && (
            <div className="text-xs text-text-tertiary">
              {notification.hostname}
            </div>
          )}
        </div>

        <div className="flex gap-1.5">
          <button
            onClick={() => void handleApprove()}
            aria-label="Approve"
            className="inline-flex h-7 w-7 items-center justify-center rounded-md text-status-online transition-colors duration-150 hover:bg-status-online/20 focus-visible:ring-2 focus-visible:ring-border-hover focus-visible:outline-none"
          >
            <Check size={16} />
          </button>
          <button
            onClick={() => void handleReject()}
            aria-label="Reject"
            className="inline-flex h-7 w-7 items-center justify-center rounded-md text-status-error transition-colors duration-150 hover:bg-status-error/20 focus-visible:ring-2 focus-visible:ring-border-hover focus-visible:outline-none"
          >
            <X size={16} />
          </button>
          <button
            onClick={handleNavigate}
            aria-label="Go to terminal"
            className="inline-flex h-7 w-7 items-center justify-center rounded-md text-text-secondary transition-colors duration-150 hover:bg-bg-hover hover:text-text-primary focus-visible:ring-2 focus-visible:ring-border-hover focus-visible:outline-none"
          >
            <Terminal size={16} />
          </button>
        </div>
      </div>

      <button
        onClick={() => { dismiss(notification.loopId); }}
        aria-label="Dismiss notification"
        className="absolute top-2 right-2 inline-flex h-5 w-5 items-center justify-center rounded text-text-tertiary transition-colors duration-150 hover:text-text-primary focus-visible:ring-2 focus-visible:ring-border-hover focus-visible:outline-none"
      >
        <X size={12} />
      </button>
    </div>
  );
}

export function ActionToastContainer() {
  const notifications = useNotificationStore((s) => s.notifications);

  const sorted = useMemo(() => {
    return [...notifications.values()].sort(
      (a, b) => a.createdAt - b.createdAt,
    );
  }, [notifications]);

  if (sorted.length === 0) return null;

  const visible = sorted.slice(0, MAX_VISIBLE);
  const overflow = sorted.length - MAX_VISIBLE;

  return (
    <div className="fixed right-4 bottom-20 z-50 flex flex-col gap-2">
      {visible.map((notif) => (
        <ActionToastItem key={notif.id} notification={notif} />
      ))}
      {overflow > 0 && (
        <div className="text-right text-xs text-text-tertiary">
          +{overflow} more
        </div>
      )}
    </div>
  );
}
