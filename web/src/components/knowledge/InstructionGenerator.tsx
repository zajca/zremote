import { useState } from "react";
import { api } from "../../lib/api";
import { Button } from "../ui/Button";

export function InstructionGenerator({ projectId }: { projectId: string }) {
  const [content, setContent] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [memoriesUsed, setMemoriesUsed] = useState(0);
  const [copied, setCopied] = useState(false);
  const [writing, setWriting] = useState(false);
  const [writeResult, setWriteResult] = useState<string | null>(null);

  const handleGenerate = async () => {
    setLoading(true);
    try {
      const data = await api.knowledge.generateInstructions(projectId);
      setContent(data.content ?? null);
      setMemoriesUsed(data.memories_used ?? 0);
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

  const handleWriteClaudeMd = async () => {
    setWriting(true);
    setWriteResult(null);
    try {
      const result = await api.knowledge.writeClaudeMd(projectId);
      setWriteResult(
        `Written to CLAUDE.md (${result.bytes} bytes)`,
      );
      setTimeout(() => setWriteResult(null), 5000);
    } catch (e) {
      setWriteResult(
        `Failed: ${e instanceof Error ? e.message : "unknown error"}`,
      );
    } finally {
      setWriting(false);
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
            <div className="flex items-center gap-2">
              <button
                onClick={handleCopy}
                className="text-xs text-text-secondary transition-colors hover:text-text-primary"
              >
                {copied ? "Copied!" : "Copy to clipboard"}
              </button>
              <Button
                variant="secondary"
                size="sm"
                onClick={handleWriteClaudeMd}
                disabled={writing}
              >
                {writing ? "Writing..." : "Write to CLAUDE.md"}
              </Button>
            </div>
          </div>
          {writeResult && (
            <div
              className={`rounded p-2 text-xs ${
                writeResult.startsWith("Failed")
                  ? "bg-status-error/10 text-status-error"
                  : "bg-status-online/10 text-status-online"
              }`}
            >
              {writeResult}
            </div>
          )}
          <pre className="max-h-[500px] overflow-auto whitespace-pre-wrap rounded-md border border-border bg-bg-tertiary p-3 font-mono text-xs text-text-secondary">
            {content}
          </pre>
        </div>
      )}
    </div>
  );
}
