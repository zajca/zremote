import { useState, useEffect } from "react";
import { useKnowledgeStore } from "../../stores/knowledge-store";
import { MemoryCard } from "./MemoryCard";
import type { MemoryCategory } from "../../types/knowledge";

const CATEGORIES: { value: MemoryCategory | "all"; label: string }[] = [
  { value: "all", label: "All" },
  { value: "pattern", label: "Patterns" },
  { value: "decision", label: "Decisions" },
  { value: "pitfall", label: "Pitfalls" },
  { value: "preference", label: "Preferences" },
  { value: "architecture", label: "Architecture" },
  { value: "convention", label: "Conventions" },
];

export function MemoryTimeline({ projectId }: { projectId: string }) {
  const [filter, setFilter] = useState<MemoryCategory | "all">("all");
  const { memoriesByProject, fetchMemories, deleteMemory, updateMemory } =
    useKnowledgeStore();

  useEffect(() => {
    fetchMemories(projectId, filter === "all" ? undefined : filter);
  }, [projectId, filter, fetchMemories]);

  const memories = memoriesByProject[projectId] ?? [];

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap gap-1">
        {CATEGORIES.map((cat) => (
          <button
            key={cat.value}
            onClick={() => setFilter(cat.value)}
            className={`rounded px-2 py-1 text-xs transition-colors ${
              filter === cat.value
                ? "bg-bg-tertiary text-text-primary"
                : "text-text-secondary hover:bg-bg-hover hover:text-text-primary"
            }`}
          >
            {cat.label}
          </button>
        ))}
      </div>

      {memories.length === 0 ? (
        <div className="py-8 text-center text-sm text-text-tertiary">
          No memories extracted yet
        </div>
      ) : (
        <div className="space-y-2">
          {memories.map((memory) => (
            <MemoryCard
              key={memory.id}
              memory={memory}
              onDelete={() => deleteMemory(projectId, memory.id)}
              onUpdate={(data) => updateMemory(projectId, memory.id, data)}
            />
          ))}
        </div>
      )}
    </div>
  );
}
