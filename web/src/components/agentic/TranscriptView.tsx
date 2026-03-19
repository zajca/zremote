import { ArrowDown } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { TranscriptEntry, TranscriptRole } from "../../types/agentic";

interface TranscriptViewProps {
  entries: TranscriptEntry[];
}

function roleStyles(role: TranscriptRole): string {
  switch (role) {
    case "assistant":
      return "self-start bg-bg-tertiary text-text-primary rounded-lg rounded-bl-sm";
    case "user":
      return "self-end bg-accent/15 text-text-primary rounded-lg rounded-br-sm";
    case "tool":
      return "self-start bg-bg-secondary text-text-secondary font-mono text-xs rounded-md border border-border";
    case "system":
      return "self-center bg-transparent text-text-tertiary text-xs italic";
  }
}

function roleLabel(role: TranscriptRole): string | null {
  switch (role) {
    case "assistant":
      return "Assistant";
    case "user":
      return "You";
    case "tool":
      return "Tool";
    case "system":
      return null;
  }
}

function formatTime(timestamp: string): string {
  try {
    return new Date(timestamp).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return "";
  }
}

export function TranscriptView({ entries }: TranscriptViewProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [autoScroll, setAutoScroll] = useState(true);
  const [showScrollButton, setShowScrollButton] = useState(false);

  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    setAutoScroll(atBottom);
    setShowScrollButton(!atBottom);
  }, []);

  const scrollToBottom = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    setAutoScroll(true);
    setShowScrollButton(false);
  }, []);

  useEffect(() => {
    if (autoScroll && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [entries.length, autoScroll]);

  if (entries.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-text-tertiary">
        No transcript entries yet
      </div>
    );
  }

  return (
    <div className="relative h-full">
      <div
        ref={scrollRef}
        onScroll={handleScroll}
        className="flex h-full flex-col gap-2 overflow-y-auto p-3"
      >
        {entries.map((entry) => {
          const label = roleLabel(entry.role);
          return (
            <div key={entry.id} className={`flex max-w-[85%] flex-col gap-0.5 px-3 py-2 ${roleStyles(entry.role)}`}>
              {label && (
                <div className="flex items-center gap-2">
                  <span className="text-xs font-medium text-text-tertiary">
                    {label}
                  </span>
                  <span className="text-[10px] text-text-tertiary">
                    {formatTime(entry.timestamp)}
                  </span>
                </div>
              )}
              <div className="whitespace-pre-wrap break-words text-sm">
                {entry.content}
              </div>
            </div>
          );
        })}
      </div>
      {showScrollButton && (
        <button
          onClick={scrollToBottom}
          className="absolute bottom-3 left-1/2 flex -translate-x-1/2 items-center gap-1 rounded-full bg-bg-tertiary px-3 py-1 text-xs text-text-secondary shadow-lg transition-colors hover:bg-bg-hover"
        >
          <ArrowDown size={12} />
          Scroll to latest
        </button>
      )}
    </div>
  );
}
