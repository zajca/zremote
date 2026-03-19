import type { IndexingProgress as IndexingProgressType } from "../../types/knowledge";

export function IndexingProgress({
  progress,
}: {
  progress: IndexingProgressType;
}) {
  const pct =
    progress.files_total > 0
      ? Math.round((progress.files_processed / progress.files_total) * 100)
      : 0;

  return (
    <div className="border-b border-accent/20 bg-accent/5 px-3 py-2">
      <div className="mb-1 flex items-center justify-between">
        <span className="text-xs text-accent">
          Indexing: {progress.status === "queued" ? "Queued" : `${pct}%`}
        </span>
        <span className="text-xs text-text-tertiary">
          {progress.files_processed}/{progress.files_total} files
        </span>
      </div>
      <div className="h-1 overflow-hidden rounded-full bg-bg-tertiary">
        <div
          className="h-full rounded-full bg-accent transition-all duration-300"
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}
