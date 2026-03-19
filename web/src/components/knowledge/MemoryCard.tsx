import { useState } from "react";
import type { KnowledgeMemory } from "../../types/knowledge";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";

function categoryBadgeVariant(
  category: string,
): "online" | "creating" | "error" | "warning" | "offline" {
  switch (category) {
    case "pattern":
      return "creating";
    case "decision":
      return "online";
    case "pitfall":
      return "error";
    case "preference":
      return "warning";
    case "architecture":
      return "creating";
    case "convention":
      return "online";
    default:
      return "offline";
  }
}

interface MemoryCardProps {
  memory: KnowledgeMemory;
  onDelete: () => void;
  onUpdate: (data: { content?: string; category?: string }) => void;
}

export function MemoryCard({ memory, onDelete, onUpdate }: MemoryCardProps) {
  const [editing, setEditing] = useState(false);
  const [editContent, setEditContent] = useState(memory.content);

  const handleSave = () => {
    if (editContent !== memory.content) {
      onUpdate({ content: editContent });
    }
    setEditing(false);
  };

  return (
    <div className="rounded-md border border-border bg-bg-secondary p-3">
      <div className="mb-2 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span className="text-xs font-medium text-text-primary">
            {memory.key}
          </span>
          <Badge variant={categoryBadgeVariant(memory.category)}>
            {memory.category}
          </Badge>
        </div>
        <div className="flex items-center gap-2">
          <span className="text-xs text-text-tertiary">
            {(memory.confidence * 100).toFixed(0)}%
          </span>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setEditing(!editing)}
          >
            {editing ? "Cancel" : "Edit"}
          </Button>
          <Button variant="danger" size="sm" onClick={onDelete}>
            Delete
          </Button>
        </div>
      </div>

      {editing ? (
        <div className="space-y-2">
          <textarea
            value={editContent}
            onChange={(e) => setEditContent(e.target.value)}
            className="min-h-[60px] w-full resize-y rounded-md border border-border bg-bg-tertiary px-2 py-1.5 text-xs text-text-primary focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
          />
          <Button variant="primary" size="sm" onClick={handleSave}>
            Save
          </Button>
        </div>
      ) : (
        <p className="whitespace-pre-wrap text-xs text-text-secondary">
          {memory.content}
        </p>
      )}

      <div className="mt-2 text-xs text-text-tertiary">
        Updated {new Date(memory.updated_at).toLocaleDateString()}
      </div>
    </div>
  );
}
