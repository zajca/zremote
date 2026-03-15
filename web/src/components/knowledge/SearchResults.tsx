import type { SearchResult } from "../../types/knowledge";
import { Badge } from "../ui/Badge";

function tierBadgeVariant(
  tier: string,
): "online" | "creating" | "warning" {
  switch (tier) {
    case "l0":
      return "online";
    case "l1":
      return "creating";
    default:
      return "warning";
  }
}

export function SearchResults({ results }: { results: SearchResult[] }) {
  if (results.length === 0) return null;

  return (
    <div className="space-y-2">
      <div className="text-xs text-text-tertiary">{results.length} results</div>
      {results.map((result, i) => (
        <div
          key={i}
          className="rounded-md border border-border bg-bg-secondary p-3"
        >
          <div className="mb-1 flex items-center justify-between">
            <span className="font-mono text-xs text-text-primary">
              {result.path}
            </span>
            <div className="flex items-center gap-2">
              {result.line_start != null && (
                <span className="text-xs text-text-tertiary">
                  L{result.line_start}
                  {result.line_end != null &&
                  result.line_end !== result.line_start
                    ? `-${result.line_end}`
                    : ""}
                </span>
              )}
              <span className="text-xs text-text-tertiary">
                {(result.score * 100).toFixed(0)}%
              </span>
              <Badge variant={tierBadgeVariant(result.tier)}>
                {result.tier.toUpperCase()}
              </Badge>
            </div>
          </div>
          <pre className="whitespace-pre-wrap font-mono text-xs text-text-secondary">
            {result.snippet}
          </pre>
        </div>
      ))}
    </div>
  );
}
