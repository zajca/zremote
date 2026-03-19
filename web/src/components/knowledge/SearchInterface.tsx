import { useState } from "react";
import { useKnowledgeStore } from "../../stores/knowledge-store";
import { SearchResults } from "./SearchResults";
import { Button } from "../ui/Button";

export function SearchInterface({ projectId }: { projectId: string }) {
  const [query, setQuery] = useState("");
  const [tier, setTier] = useState("l1");
  const { search, searchResults, searchLoading } = useKnowledgeStore();

  const handleSearch = () => {
    if (query.trim()) {
      search(projectId, query.trim(), tier);
    }
  };

  return (
    <div className="space-y-3">
      <div className="flex gap-2">
        <input
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleSearch()}
          placeholder="Search project knowledge..."
          className="h-8 flex-1 rounded-md border border-border bg-bg-tertiary px-3 text-sm text-text-primary transition-colors duration-150 placeholder:text-text-tertiary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
        />
        <select
          value={tier}
          onChange={(e) => setTier(e.target.value)}
          className="h-8 rounded-md border border-border bg-bg-tertiary px-2 text-xs text-text-secondary"
        >
          <option value="l0">L0 - Exact</option>
          <option value="l1">L1 - Semantic</option>
          <option value="l2">L2 - Exploratory</option>
        </select>
        <Button
          variant="primary"
          size="sm"
          onClick={handleSearch}
          disabled={searchLoading || !query.trim()}
        >
          {searchLoading ? "Searching..." : "Search"}
        </Button>
      </div>

      <SearchResults results={searchResults} />
    </div>
  );
}
