interface CostTrackerProps {
  costUsd: number;
  tokensIn: number;
  tokensOut: number;
  compact?: boolean;
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

function formatCost(usd: number): string {
  return `$${usd.toFixed(2)}`;
}

export function CostTracker({
  costUsd,
  tokensIn,
  tokensOut,
  compact = false,
}: CostTrackerProps) {
  if (compact) {
    return (
      <span
        className="text-xs font-medium text-text-primary"
        title={`${formatTokens(tokensIn)} in / ${formatTokens(tokensOut)} out`}
      >
        {formatCost(costUsd)}
      </span>
    );
  }

  return (
    <div className="flex items-center gap-1.5 text-xs text-text-secondary">
      <span className="font-medium text-text-primary">
        {formatCost(costUsd)}
      </span>
      <span className="text-text-tertiary">&middot;</span>
      <span>
        {formatTokens(tokensIn)} in / {formatTokens(tokensOut)} out
      </span>
    </div>
  );
}
