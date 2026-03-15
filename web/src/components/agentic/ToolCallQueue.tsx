import { Check, Clock, Loader2, X } from "lucide-react";
import type { ToolCall, ToolCallStatus } from "../../types/agentic";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";

interface ToolCallQueueProps {
  toolCalls: ToolCall[];
  onApprove: (toolCallId: string) => void;
  onReject: (toolCallId: string) => void;
}

function statusBadgeVariant(
  status: ToolCallStatus,
): "online" | "offline" | "error" | "warning" | "creating" {
  switch (status) {
    case "Completed":
      return "online";
    case "Running":
    case "Approved":
      return "creating";
    case "Pending":
      return "warning";
    case "Rejected":
    case "Failed":
      return "error";
    default:
      return "offline";
  }
}

function StatusIcon({ status }: { status: ToolCallStatus }) {
  switch (status) {
    case "Running":
      return <Loader2 size={12} className="animate-spin text-accent" />;
    case "Completed":
      return <Check size={12} className="text-status-online" />;
    case "Failed":
      return <X size={12} className="text-status-error" />;
    case "Pending":
      return <Clock size={12} className="text-status-warning" />;
    default:
      return null;
  }
}

function truncateArgs(json: string | null, maxLen = 80): string {
  if (!json) return "";
  if (json.length <= maxLen) return json;
  return json.slice(0, maxLen) + "...";
}

function formatDuration(ms: number | null): string {
  if (ms === null) return "";
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

export function ToolCallQueue({
  toolCalls,
  onApprove,
  onReject,
}: ToolCallQueueProps) {
  const pending = toolCalls.filter((tc) => tc.status === "Pending");
  const running = toolCalls.filter((tc) => tc.status === "Running");
  const history = toolCalls.filter(
    (tc) =>
      tc.status !== "Pending" && tc.status !== "Running",
  );

  return (
    <div className="flex h-full flex-col overflow-hidden">
      {/* Pending section */}
      {pending.length > 0 && (
        <div className="border-b border-border">
          <div className="px-3 py-1.5 text-xs font-medium text-status-warning">
            Pending ({pending.length})
          </div>
          <div className="space-y-px">
            {pending.map((tc) => (
              <div
                key={tc.id}
                className="flex items-center gap-2 border-l-2 border-status-warning bg-status-warning/5 px-3 py-2"
              >
                <StatusIcon status={tc.status} />
                <span className="shrink-0 text-sm font-medium text-text-primary">
                  {tc.tool_name}
                </span>
                <span className="min-w-0 flex-1 truncate font-mono text-xs text-text-tertiary">
                  {truncateArgs(tc.arguments_json)}
                </span>
                <div className="flex shrink-0 items-center gap-1">
                  <Button
                    size="sm"
                    variant="primary"
                    onClick={() => onApprove(tc.id)}
                  >
                    <Check size={12} />
                    Approve
                  </Button>
                  <Button
                    size="sm"
                    variant="danger"
                    onClick={() => onReject(tc.id)}
                  >
                    <X size={12} />
                    Reject
                  </Button>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Running section */}
      {running.length > 0 && (
        <div className="border-b border-border">
          <div className="px-3 py-1.5 text-xs font-medium text-accent">
            Running ({running.length})
          </div>
          <div className="space-y-px">
            {running.map((tc) => (
              <div
                key={tc.id}
                className="flex items-center gap-2 border-l-2 border-accent bg-accent/5 px-3 py-2"
              >
                <StatusIcon status={tc.status} />
                <span className="shrink-0 text-sm font-medium text-text-primary">
                  {tc.tool_name}
                </span>
                <span className="min-w-0 flex-1 truncate font-mono text-xs text-text-tertiary">
                  {truncateArgs(tc.arguments_json)}
                </span>
                <Badge variant="creating">{tc.status}</Badge>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* History section */}
      <div className="flex-1 overflow-y-auto">
        {history.length > 0 && (
          <>
            <div className="px-3 py-1.5 text-xs font-medium text-text-tertiary">
              History ({history.length})
            </div>
            <div className="space-y-px">
              {history.map((tc) => (
                <div
                  key={tc.id}
                  className="flex items-center gap-2 px-3 py-1.5"
                >
                  <StatusIcon status={tc.status} />
                  <span className="shrink-0 text-sm text-text-secondary">
                    {tc.tool_name}
                  </span>
                  <Badge variant={statusBadgeVariant(tc.status)}>
                    {tc.status}
                  </Badge>
                  {tc.duration_ms !== null && (
                    <span className="text-xs text-text-tertiary">
                      {formatDuration(tc.duration_ms)}
                    </span>
                  )}
                  {tc.result_preview && (
                    <span className="min-w-0 flex-1 truncate font-mono text-xs text-text-tertiary">
                      {tc.result_preview}
                    </span>
                  )}
                </div>
              ))}
            </div>
          </>
        )}
        {pending.length === 0 &&
          running.length === 0 &&
          history.length === 0 && (
            <div className="flex h-full items-center justify-center text-sm text-text-tertiary">
              No tool calls yet
            </div>
          )}
      </div>
    </div>
  );
}
