import { useCallback, useEffect, useRef, useState } from "react";
import { Clock, Filter, Search, X } from "lucide-react";
import { format } from "date-fns";
import { Button } from "../components/ui/Button";
import { Input } from "../components/ui/Input";
import { Badge } from "../components/ui/Badge";

interface SearchResult {
  transcript_id: number;
  loop_id: string;
  role: string;
  content: string;
  timestamp: string;
  tool_name: string;
  project_path: string | null;
  loop_status: string;
  model: string | null;
  estimated_cost_usd: number | null;
}

interface SearchResponse {
  results: SearchResult[];
  total: number;
  page: number;
  per_page: number;
}

function statusVariant(
  status: string,
): "online" | "offline" | "error" | "warning" | "creating" {
  switch (status) {
    case "completed":
      return "online";
    case "error":
      return "error";
    case "working":
      return "creating";
    case "paused":
      return "warning";
    default:
      return "offline";
  }
}

function highlightMatch(text: string, query: string): React.ReactNode {
  if (!query.trim()) return text;
  const words = query
    .trim()
    .split(/\s+/)
    .filter((w) => w.length > 0);
  if (words.length === 0) return text;

  const pattern = new RegExp(
    `(${words.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")})`,
    "gi",
  );
  const parts = text.split(pattern);
  return parts.map((part, i) =>
    pattern.test(part) ? (
      <mark
        key={i}
        className="rounded bg-accent/30 px-0.5 text-text-primary"
      >
        {part}
      </mark>
    ) : (
      part
    ),
  );
}

export function HistoryBrowser() {
  const [query, setQuery] = useState("");
  const [host, setHost] = useState("");
  const [project, setProject] = useState("");
  const [results, setResults] = useState<SearchResult[]>([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [loading, setLoading] = useState(false);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const perPage = 20;
  const totalPages = Math.max(1, Math.ceil(total / perPage));

  const fetchResults = useCallback(
    async (searchQuery: string, p: number) => {
      setLoading(true);
      const params = new URLSearchParams();
      if (searchQuery.trim()) params.set("q", searchQuery.trim());
      if (host.trim()) params.set("host", host.trim());
      if (project.trim()) params.set("project", project.trim());
      params.set("page", String(p));
      params.set("per_page", String(perPage));

      try {
        const resp = await fetch(`/api/search/transcripts?${params}`);
        const data = (await resp.json()) as SearchResponse;
        setResults(data.results);
        setTotal(data.total);
      } catch (e) {
        console.error("Search failed", e);
      } finally {
        setLoading(false);
      }
    },
    [host, project],
  );

  // Debounce search input
  useEffect(() => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      setPage(1);
      void fetchResults(query, 1);
    }, 300);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [query, fetchResults]);

  // Immediate fetch on filter change
  useEffect(() => {
    setPage(1);
    void fetchResults(query, 1);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host, project]);

  const handlePageChange = useCallback(
    (newPage: number) => {
      setPage(newPage);
      void fetchResults(query, newPage);
    },
    [query, fetchResults],
  );

  const selectedResult = results.find((r) => r.transcript_id === selectedId);

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-3 border-b border-border px-6 py-4">
        <Clock size={20} className="text-accent" />
        <h1 className="text-lg font-semibold text-text-primary">History</h1>
      </div>

      {/* Search + filters */}
      <div className="flex flex-wrap items-end gap-3 border-b border-border px-6 py-3">
        <div className="relative flex-1">
          <Search
            size={14}
            className="absolute top-1/2 left-2.5 -translate-y-1/2 text-text-tertiary"
          />
          <input
            type="text"
            placeholder="Search transcripts..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="h-8 w-full rounded-md border border-border bg-bg-tertiary py-1 pr-3 pl-8 text-sm text-text-primary transition-colors duration-150 placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
          />
        </div>
        <div className="flex items-center gap-2">
          <Filter size={14} className="text-text-tertiary" />
          <Input
            placeholder="Host"
            value={host}
            onChange={(e) => setHost(e.target.value)}
            className="w-32"
          />
          <Input
            placeholder="Project"
            value={project}
            onChange={(e) => setProject(e.target.value)}
            className="w-40"
          />
          {(query || host || project) && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                setQuery("");
                setHost("");
                setProject("");
              }}
            >
              <X size={14} />
              Clear
            </Button>
          )}
        </div>
      </div>

      <div className="flex min-h-0 flex-1">
        {/* Results list */}
        <div className="flex w-full flex-col border-r border-border lg:w-1/2">
          <div className="flex-1 overflow-auto">
            {loading ? (
              <div className="p-4 text-sm text-text-tertiary">Searching...</div>
            ) : results.length === 0 ? (
              <div className="flex flex-col items-center gap-2 pt-12 text-center">
                <Search size={24} className="text-text-tertiary" />
                <p className="text-sm text-text-secondary">No results found</p>
                {(query || host || project) && (
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => {
                      setQuery("");
                      setHost("");
                      setProject("");
                    }}
                  >
                    Reset filters
                  </Button>
                )}
              </div>
            ) : (
              results.map((result) => (
                <button
                  key={result.transcript_id}
                  onClick={() => setSelectedId(result.transcript_id)}
                  className={`flex w-full flex-col gap-1 border-b border-border px-4 py-3 text-left transition-colors duration-150 hover:bg-bg-hover ${selectedId === result.transcript_id ? "bg-bg-active" : ""}`}
                >
                  <div className="flex items-center gap-2">
                    <span className="text-xs font-medium text-text-secondary">
                      {result.role}
                    </span>
                    <Badge variant={statusVariant(result.loop_status)}>
                      {result.loop_status}
                    </Badge>
                    {result.model && (
                      <span className="text-xs text-text-tertiary">
                        {result.model}
                      </span>
                    )}
                    <span className="ml-auto text-xs text-text-tertiary">
                      {format(new Date(result.timestamp), "MMM d, HH:mm")}
                    </span>
                  </div>
                  <div className="line-clamp-2 text-sm text-text-primary">
                    {highlightMatch(result.content, query)}
                  </div>
                  <div className="flex items-center gap-2 text-xs text-text-tertiary">
                    <span>{result.tool_name}</span>
                    {result.project_path && (
                      <>
                        <span className="text-border">|</span>
                        <span className="truncate">{result.project_path}</span>
                      </>
                    )}
                    {result.estimated_cost_usd != null && (
                      <>
                        <span className="text-border">|</span>
                        <span>${result.estimated_cost_usd.toFixed(4)}</span>
                      </>
                    )}
                  </div>
                </button>
              ))
            )}
          </div>

          {/* Pagination */}
          {total > perPage && (
            <div className="flex items-center justify-between border-t border-border px-4 py-2">
              <span className="text-xs text-text-tertiary">
                {total} results
              </span>
              <div className="flex items-center gap-1">
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={page <= 1}
                  onClick={() => handlePageChange(page - 1)}
                >
                  Prev
                </Button>
                <span className="px-2 text-xs text-text-secondary">
                  {page} / {totalPages}
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={page >= totalPages}
                  onClick={() => handlePageChange(page + 1)}
                >
                  Next
                </Button>
              </div>
            </div>
          )}
        </div>

        {/* Detail panel */}
        <div className="hidden flex-1 overflow-auto lg:block">
          {selectedResult ? (
            <div className="p-6">
              <div className="mb-4 flex items-center gap-2">
                <Badge variant={statusVariant(selectedResult.loop_status)}>
                  {selectedResult.loop_status}
                </Badge>
                <span className="text-xs text-text-tertiary">
                  {format(
                    new Date(selectedResult.timestamp),
                    "PPpp",
                  )}
                </span>
              </div>
              <div className="mb-2 flex items-center gap-2 text-xs text-text-secondary">
                <span>Role: {selectedResult.role}</span>
                <span className="text-border">|</span>
                <span>Tool: {selectedResult.tool_name}</span>
                {selectedResult.model && (
                  <>
                    <span className="text-border">|</span>
                    <span>Model: {selectedResult.model}</span>
                  </>
                )}
              </div>
              <div className="whitespace-pre-wrap rounded-lg border border-border bg-bg-tertiary p-4 font-mono text-sm text-text-primary">
                {highlightMatch(selectedResult.content, query)}
              </div>
            </div>
          ) : (
            <div className="flex h-full items-center justify-center text-sm text-text-tertiary">
              Select a result to view details
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
