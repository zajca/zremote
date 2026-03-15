import { useEffect, useState } from "react";
import { useKnowledgeStore } from "../../stores/knowledge-store";
import { KnowledgeStatus } from "./KnowledgeStatus";
import { SearchInterface } from "./SearchInterface";
import { MemoryTimeline } from "./MemoryTimeline";
import { InstructionGenerator } from "./InstructionGenerator";
import { IndexingProgress } from "./IndexingProgress";

type Tab = "status" | "search" | "memories" | "instructions";

export function KnowledgePanel({
  projectId,
  hostId,
}: {
  projectId: string;
  hostId: string;
}) {
  const [activeTab, setActiveTab] = useState<Tab>("status");
  const { fetchStatus, fetchMemories, indexingProgress } =
    useKnowledgeStore();

  useEffect(() => {
    fetchStatus(projectId);
    fetchMemories(projectId);
  }, [projectId, fetchStatus, fetchMemories]);

  const tabs: { id: Tab; label: string }[] = [
    { id: "status", label: "Status" },
    { id: "search", label: "Search" },
    { id: "memories", label: "Memories" },
    { id: "instructions", label: "Instructions" },
  ];

  const progress = indexingProgress[projectId];

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-1 border-b border-border px-3 py-2">
        <span className="mr-3 text-sm font-medium text-text-primary">
          Knowledge
        </span>
        {tabs.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            className={`rounded px-3 py-1 text-xs transition-colors ${
              activeTab === tab.id
                ? "bg-bg-tertiary text-text-primary"
                : "text-text-secondary hover:bg-bg-hover hover:text-text-primary"
            }`}
          >
            {tab.label}
          </button>
        ))}
      </div>

      {progress &&
        (progress.status === "queued" || progress.status === "in_progress") && (
          <IndexingProgress progress={progress} />
        )}

      <div className="flex-1 overflow-auto p-3">
        {activeTab === "status" && (
          <KnowledgeStatus projectId={projectId} hostId={hostId} />
        )}
        {activeTab === "search" && <SearchInterface projectId={projectId} />}
        {activeTab === "memories" && <MemoryTimeline projectId={projectId} />}
        {activeTab === "instructions" && (
          <InstructionGenerator projectId={projectId} />
        )}
      </div>
    </div>
  );
}
