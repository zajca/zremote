import { Bot, Clock } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useAgenticStore } from "../../stores/agentic-store";
import type {
  AgenticStatus,
  ToolCall,
  TranscriptEntry,
  UserAction,
} from "../../types/agentic";
import { Badge } from "../ui/Badge";
import { AgenticActionBar } from "./AgenticActionBar";
import { ContextUsageBar } from "./ContextUsageBar";
import { CostTracker } from "./CostTracker";
import { ToolCallQueue } from "./ToolCallQueue";
import { TranscriptView } from "./TranscriptView";
import { showToast } from "../layout/Toast";

interface AgenticLoopPanelProps {
  loopId: string;
}

type TabId = "tools" | "transcript";

const EMPTY_TOOL_CALLS: ToolCall[] = [];
const EMPTY_TRANSCRIPT: TranscriptEntry[] = [];

function statusBadgeVariant(
  status: AgenticStatus,
): "online" | "offline" | "error" | "warning" | "creating" {
  switch (status) {
    case "working":
      return "creating";
    case "waiting_for_input":
      return "warning";
    case "paused":
      return "offline";
    case "error":
      return "error";
    case "completed":
      return "online";
  }
}

function useDurationTimer(startedAt: string, endedAt: string | null): string {
  const [elapsed, setElapsed] = useState("");

  useEffect(() => {
    function update() {
      const start = new Date(startedAt).getTime();
      const end = endedAt ? new Date(endedAt).getTime() : Date.now();
      const diffMs = Math.max(0, end - start);
      const seconds = Math.floor(diffMs / 1000) % 60;
      const minutes = Math.floor(diffMs / 60000) % 60;
      const hours = Math.floor(diffMs / 3600000);

      if (hours > 0) {
        setElapsed(`${hours}h ${minutes}m ${seconds}s`);
      } else if (minutes > 0) {
        setElapsed(`${minutes}m ${seconds}s`);
      } else {
        setElapsed(`${seconds}s`);
      }
    }

    update();
    if (!endedAt) {
      const interval = setInterval(update, 1000);
      return () => clearInterval(interval);
    }
  }, [startedAt, endedAt]);

  return elapsed;
}

export function AgenticLoopPanel({ loopId }: AgenticLoopPanelProps) {
  const [activeTab, setActiveTab] = useState<TabId>("tools");
  const loop = useAgenticStore((s) => s.activeLoops.get(loopId));
  const toolCalls =
    useAgenticStore((s) => s.toolCalls.get(loopId)) ?? EMPTY_TOOL_CALLS;
  const transcript =
    useAgenticStore((s) => s.transcripts.get(loopId)) ?? EMPTY_TRANSCRIPT;

  useEffect(() => {
    const store = useAgenticStore.getState();
    void store.fetchLoop(loopId);
    void store.fetchToolCalls(loopId);
    void store.fetchTranscript(loopId);
  }, [loopId]);

  const duration = useDurationTimer(
    loop?.started_at ?? new Date().toISOString(),
    loop?.ended_at ?? null,
  );

  const handleAction = useCallback(
    (action: UserAction, payload?: string) => {
      useAgenticStore
        .getState()
        .sendAction(loopId, action, payload)
        .catch(() => showToast("Failed to send action", "error"));
    },
    [loopId],
  );

  const handleToolApprove = useCallback(
    (_toolCallId: string) => {
      useAgenticStore
        .getState()
        .sendAction(loopId, "approve")
        .catch(() => showToast("Failed to approve tool", "error"));
    },
    [loopId],
  );

  const handleToolReject = useCallback(
    (_toolCallId: string) => {
      useAgenticStore
        .getState()
        .sendAction(loopId, "reject")
        .catch(() => showToast("Failed to reject tool", "error"));
    },
    [loopId],
  );

  // Tab switching via number keys
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement
      ) {
        return;
      }
      // Don't capture shortcuts when terminal has focus
      const target = e.target as HTMLElement;
      if (target.closest('.xterm')) return;

      switch (e.key) {
        case "1":
          setActiveTab("tools");
          break;
        case "2":
          setActiveTab("transcript");
          break;
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  if (!loop) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-text-tertiary">
        Loading loop...
      </div>
    );
  }

  const tabs: { id: TabId; label: string; shortcut: string }[] = [
    { id: "tools", label: "Tool Queue", shortcut: "1" },
    { id: "transcript", label: "Transcript", shortcut: "2" },
  ];

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="flex items-center gap-3 border-b border-border px-4 py-2">
        <Bot size={18} className="shrink-0 text-accent" />
        <span className="text-sm font-semibold text-text-primary">
          {loop.task_name || loop.tool_name}
        </span>
        {loop.task_name && (
          <span className="text-xs text-text-tertiary">{loop.tool_name}</span>
        )}
        <Badge variant={statusBadgeVariant(loop.status)}>{loop.status}</Badge>
        <div className="flex items-center gap-1 text-xs text-text-tertiary">
          <Clock size={12} />
          {duration}
        </div>
        <div className="ml-auto flex items-center gap-4">
          <CostTracker
            costUsd={loop.estimated_cost_usd}
            tokensIn={loop.total_tokens_in}
            tokensOut={loop.total_tokens_out}
          />
          <div className="w-48">
            <ContextUsageBar
              used={loop.context_used}
              max={loop.context_max}
            />
          </div>
        </div>
      </div>

      {/* Action Bar */}
      <AgenticActionBar status={loop.status} onAction={handleAction} />

      {/* Tab Switcher */}
      <div className="flex border-b border-border">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            className={`flex items-center gap-1.5 px-4 py-2 text-sm transition-colors ${
              activeTab === tab.id
                ? "border-b-2 border-accent text-text-primary"
                : "text-text-secondary hover:text-text-primary"
            }`}
          >
            {tab.label}
            <kbd className="rounded bg-bg-tertiary px-1 py-0.5 text-[10px] text-text-tertiary">
              {tab.shortcut}
            </kbd>
            {tab.id === "tools" && loop.pending_tool_calls > 0 && (
              <span className="ml-1 inline-flex h-4 min-w-[16px] items-center justify-center rounded-full bg-status-warning/20 px-1 text-[10px] font-medium text-status-warning">
                {loop.pending_tool_calls}
              </span>
            )}
          </button>
        ))}
      </div>

      {/* Content Area */}
      <div className="min-h-0 flex-1">
        {activeTab === "tools" && (
          <ToolCallQueue
            toolCalls={toolCalls}
            onApprove={handleToolApprove}
            onReject={handleToolReject}
          />
        )}
        {activeTab === "transcript" && (
          <TranscriptView entries={transcript} />
        )}
      </div>
    </div>
  );
}
