import { useState } from "react";
import { Button } from "../ui/Button";

export function InstructionGenerator({ projectId }: { projectId: string }) {
  const [content, setContent] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [memoriesUsed, setMemoriesUsed] = useState(0);
  const [copied, setCopied] = useState(false);

  const handleGenerate = async () => {
    setLoading(true);
    try {
      const response = await fetch(
        `/api/projects/${projectId}/knowledge/generate-instructions`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
        },
      );
      if (response.ok) {
        const data = (await response.json()) as {
          content?: string;
          memories_used?: number;
        };
        setContent(data.content ?? null);
        setMemoriesUsed(data.memories_used ?? 0);
      }
    } catch (e) {
      console.error("Failed to generate instructions:", e);
    } finally {
      setLoading(false);
    }
  };

  const handleCopy = () => {
    if (content) {
      void navigator.clipboard.writeText(content);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-xs text-text-secondary">
          Generate project instructions from extracted memories.
        </p>
        <Button
          variant="primary"
          size="sm"
          onClick={handleGenerate}
          disabled={loading}
        >
          {loading ? "Generating..." : "Generate"}
        </Button>
      </div>

      {content && (
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <span className="text-xs text-text-tertiary">
              Based on {memoriesUsed} memories
            </span>
            <button
              onClick={handleCopy}
              className="text-xs text-text-secondary transition-colors hover:text-text-primary"
            >
              {copied ? "Copied!" : "Copy to clipboard"}
            </button>
          </div>
          <pre className="max-h-[500px] overflow-auto whitespace-pre-wrap rounded-md border border-border bg-bg-tertiary p-3 font-mono text-xs text-text-secondary">
            {content}
          </pre>
        </div>
      )}
    </div>
  );
}
