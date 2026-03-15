interface CostTrackerProps {
  costUsd: number;
  tokensIn: number;
  tokensOut: number;
  model: string | null;
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
  model,
}: CostTrackerProps) {
  return (
    <div className="flex items-center gap-1.5 text-xs text-text-secondary">
      <span className="font-medium text-text-primary">
        {formatCost(costUsd)}
      </span>
      <span className="text-text-tertiary">|</span>
      <span>
        {formatTokens(tokensIn)} in / {formatTokens(tokensOut)} out
      </span>
      {model && (
        <>
          <span className="text-text-tertiary">|</span>
          <span className="text-text-tertiary">{model}</span>
        </>
      )}
    </div>
  );
}
