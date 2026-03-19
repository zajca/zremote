type StatusDotStatus = "online" | "offline" | "error";

interface StatusDotProps {
  status: StatusDotStatus;
  pulse?: boolean;
}

const statusColors: Record<StatusDotStatus, string> = {
  online: "bg-status-online",
  offline: "bg-status-offline",
  error: "bg-status-error",
};

export function StatusDot({ status, pulse = false }: StatusDotProps) {
  return (
    <span className="relative inline-flex h-2 w-2 shrink-0">
      {pulse && (
        <span
          className={`absolute inline-flex h-full w-full animate-ping rounded-full opacity-75 ${statusColors[status]}`}
        />
      )}
      <span
        className={`relative inline-flex h-2 w-2 rounded-full ${statusColors[status]}`}
      />
    </span>
  );
}
