export interface KnowledgeBase {
  id: string;
  host_id: string;
  status: "starting" | "ready" | "indexing" | "error" | "stopped";
  openviking_version: string | null;
  last_error: string | null;
  started_at: string | null;
  updated_at: string;
}

export interface KnowledgeMemory {
  id: string;
  project_id: string;
  loop_id: string | null;
  key: string;
  content: string;
  category:
    | "pattern"
    | "decision"
    | "pitfall"
    | "preference"
    | "architecture"
    | "convention";
  confidence: number;
  created_at: string;
  updated_at: string;
}

export interface SearchResult {
  path: string;
  score: number;
  snippet: string;
  line_start: number | null;
  line_end: number | null;
  tier: "l0" | "l1" | "l2";
}

export interface SearchResponse {
  results: SearchResult[];
  duration_ms: number;
}

export interface IndexingProgress {
  project_id: string;
  project_path: string;
  status: "queued" | "in_progress" | "completed" | "failed";
  files_processed: number;
  files_total: number;
}

export type MemoryCategory = KnowledgeMemory["category"];
export type KnowledgeServiceStatus = KnowledgeBase["status"];
export type SearchTier = SearchResult["tier"];
