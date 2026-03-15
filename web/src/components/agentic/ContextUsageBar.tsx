import { AlertTriangle } from "lucide-react";

interface ContextUsageBarProps {
  used: number;
  max: number;
}

function formatTokenCount(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

export function ContextUsageBar({ used, max }: ContextUsageBarProps) {
  const pct = max > 0 ? Math.min((used / max) * 100, 100) : 0;

  const barColor =
    pct >= 85
      ? "bg-status-error"
      : pct >= 70
        ? "bg-status-warning"
        : "bg-status-online";

  return (
    <div className="flex items-center gap-2">
      <div className="h-2 min-w-[100px] flex-1 rounded-full bg-bg-tertiary">
        <div
          className={`h-full rounded-full transition-all duration-300 ${barColor}`}
          style={{ width: `${pct}%` }}
        />
      </div>
      <span className="shrink-0 text-xs text-text-secondary">
        {formatTokenCount(used)} / {formatTokenCount(max)} ({Math.round(pct)}%)
      </span>
      {pct >= 85 && (
        <AlertTriangle size={14} className="shrink-0 text-status-error" />
      )}
    </div>
  );
}
