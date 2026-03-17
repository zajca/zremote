import {
  Check,
  ChevronDown,
  ChevronUp,
  MessageSquare,
  Pause,
  Play,
  Square,
  X,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useAgenticStore } from "../../stores/agentic-store";
import type {
  AgenticStatus,
  ToolCall,
  TranscriptEntry,
  UserAction,
} from "../../types/agentic";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import { IconButton } from "../ui/IconButton";
import { ContextUsageBar } from "./ContextUsageBar";
import { CostTracker } from "./CostTracker";
import { ToolCallQueue } from "./ToolCallQueue";
import { TranscriptView } from "./TranscriptView";
import { showToast } from "../layout/Toast";

interface AgenticOverlayProps {
  loopId: string;
}

type OverlayTab = "tools" | "transcript";

const EMPTY_TOOL_CALLS: ToolCall[] = [];
const EMPTY_TRANSCRIPT: TranscriptEntry[] = [];
const OVERLAY_HEIGHT_KEY = "zremote:overlay-height-pct";
const DEFAULT_HEIGHT_PCT = 50;
const MIN_HEIGHT_PCT = 15;
const MAX_HEIGHT_PCT = 85;

function statusDotColor(status: AgenticStatus): string {
  switch (status) {
    case "working":
      return "bg-accent animate-pulse";
    case "waiting_for_input":
      return "bg-status-warning";
    case "paused":
      return "bg-text-tertiary";
    case "error":
      return "bg-status-error";
    case "completed":
      return "bg-status-online";
  }
}

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

function getPersistedHeight(): number {
  try {
    const val = localStorage.getItem(OVERLAY_HEIGHT_KEY);
    if (val) {
      const n = Number(val);
      if (n >= MIN_HEIGHT_PCT && n <= MAX_HEIGHT_PCT) return n;
    }
  } catch {
    // ignore
  }
  return DEFAULT_HEIGHT_PCT;
}

export function AgenticOverlay({ loopId }: AgenticOverlayProps) {
  const [expanded, setExpanded] = useState(false);
  const [activeTab, setActiveTab] = useState<OverlayTab>("tools");
  const [heightPct, setHeightPct] = useState(getPersistedHeight);
  const [showInput, setShowInput] = useState(false);
  const [inputValue, setInputValue] = useState("");
  const [pendingAction, setPendingAction] = useState<UserAction | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const dragging = useRef(false);

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

  // Actions
  const handleAction = useCallback(
    (action: UserAction, payload?: string) => {
      setPendingAction(action);
      useAgenticStore
        .getState()
        .sendAction(loopId, action, payload)
        .catch(() => showToast("Failed to send action", "error"));
      setTimeout(() => setPendingAction(null), 500);
    },
    [loopId],
  );

  const handleApprove = useCallback(() => {
    if (loop?.status === "waiting_for_input") handleAction("approve");
  }, [loop?.status, handleAction]);

  const handleReject = useCallback(() => {
    if (loop?.status === "waiting_for_input") handleAction("reject");
  }, [loop?.status, handleAction]);

  const handleProvideInput = useCallback(() => {
    setShowInput(true);
    setInputValue("");
  }, []);

  const handleSubmitInput = useCallback(() => {
    if (inputValue.trim()) {
      handleAction("provide_input", inputValue.trim());
      setShowInput(false);
      setInputValue("");
    }
  }, [inputValue, handleAction]);

  const handlePauseResume = useCallback(() => {
    if (loop?.status === "working") handleAction("pause");
    else if (loop?.status === "paused") handleAction("resume");
  }, [loop?.status, handleAction]);

  const handleStop = useCallback(() => {
    const isActive =
      loop?.status === "working" ||
      loop?.status === "waiting_for_input" ||
      loop?.status === "paused";
    if (isActive && window.confirm("Stop this agentic loop?")) {
      handleAction("stop");
    }
  }, [loop?.status, handleAction]);

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

  // Persist overlay height
  useEffect(() => {
    try {
      localStorage.setItem(OVERLAY_HEIGHT_KEY, String(heightPct));
    } catch {
      // ignore
    }
  }, [heightPct]);

  // Drag handle for overlay height
  const handleDragStart = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    dragging.current = true;

    const onMouseMove = (ev: MouseEvent) => {
      if (!dragging.current || !containerRef.current) return;
      const rect = containerRef.current.getBoundingClientRect();
      const pct = ((ev.clientY - rect.top) / rect.height) * 100;
      setHeightPct(Math.min(MAX_HEIGHT_PCT, Math.max(MIN_HEIGHT_PCT, pct)));
    };

    const onMouseUp = () => {
      dragging.current = false;
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
    };

    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);
  }, []);

  // Keyboard shortcuts
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement
      ) {
        return;
      }
      const target = e.target as HTMLElement;
      if (target.closest(".xterm")) return;

      const isWaiting = loop?.status === "waiting_for_input";
      const isWorking = loop?.status === "working";
      const isPaused = loop?.status === "paused";
      const isActive = isWaiting || isWorking || isPaused;

      switch (e.key) {
        case "Enter":
          if (isWaiting) {
            e.preventDefault();
            handleApprove();
          }
          break;
        case "Escape":
          if (showInput) {
            setShowInput(false);
          } else if (isWaiting) {
            e.preventDefault();
            handleReject();
          }
          break;
        case "i":
        case "I":
          if (isWaiting) {
            e.preventDefault();
            handleProvideInput();
          }
          break;
        case "p":
        case "P":
          if (isWorking || isPaused) {
            e.preventDefault();
            handlePauseResume();
          }
          break;
        case "S":
          if (e.shiftKey && isActive) {
            e.preventDefault();
            handleStop();
          }
          break;
        case "1":
          if (expanded) setActiveTab("tools");
          break;
        case "2":
          if (expanded) setActiveTab("transcript");
          break;
        case "`":
          e.preventDefault();
          setExpanded((prev) => !prev);
          break;
      }

      if (e.ctrlKey && e.shiftKey && e.key === "A") {
        e.preventDefault();
        setExpanded((prev) => !prev);
      }
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [
    loop?.status,
    expanded,
    showInput,
    handleApprove,
    handleReject,
    handleProvideInput,
    handlePauseResume,
    handleStop,
  ]);

  if (!loop) return null;

  const isWaiting = loop.status === "waiting_for_input";
  const isWorking = loop.status === "working";
  const isPaused = loop.status === "paused";
  const isActive = isWaiting || isWorking || isPaused;

  const tabs: { id: OverlayTab; label: string; shortcut: string; count?: number }[] = [
    {
      id: "tools",
      label: "Tool Queue",
      shortcut: "1",
      count: loop.pending_tool_calls > 0 ? loop.pending_tool_calls : undefined,
    },
    { id: "transcript", label: "Transcript", shortcut: "2" },
  ];

  return (
    <>
      {/* Header bar */}
      <div
        className={`z-20 flex h-9 shrink-0 items-center gap-2 border-b px-3 ${
          isWaiting
            ? "border-status-warning/30 bg-status-warning/5"
            : "border-border bg-bg-secondary"
        }`}
      >
        {/* Status dot + tool name + badge + duration */}
        <div className={`h-2 w-2 shrink-0 rounded-full ${statusDotColor(loop.status)}`} />
        <span className="text-xs font-medium text-text-primary">
          {loop.tool_name}
        </span>
        <Badge variant={statusBadgeVariant(loop.status)}>
          {loop.status === "waiting_for_input" ? "waiting" : loop.status}
        </Badge>
        <span className="text-xs text-text-tertiary">{duration}</span>

        {/* Action buttons (visible when waiting/paused) */}
        {isWaiting && (
          <div className="ml-2 flex items-center gap-1">
            <Button
              size="sm"
              variant="primary"
              disabled={pendingAction === "approve"}
              onClick={handleApprove}
              className="h-6 bg-status-online px-2 text-[11px] hover:bg-status-online/80"
            >
              <Check size={12} />
              Approve
            </Button>
            <Button
              size="sm"
              variant="danger"
              disabled={pendingAction === "reject"}
              onClick={handleReject}
              className="h-6 px-2 text-[11px]"
            >
              <X size={12} />
              Reject
            </Button>
            <Button
              size="sm"
              variant="secondary"
              onClick={handleProvideInput}
              className="h-6 px-2 text-[11px]"
            >
              <MessageSquare size={12} />
              Input
            </Button>
          </div>
        )}

        {isPaused && (
          <div className="ml-2 flex items-center gap-1">
            <Button
              size="sm"
              variant="ghost"
              onClick={handlePauseResume}
              className="h-6 px-2 text-[11px]"
            >
              <Play size={12} />
              Resume
            </Button>
            <Button
              size="sm"
              variant="danger"
              disabled={!isActive}
              onClick={handleStop}
              className="h-6 px-2 text-[11px]"
            >
              <Square size={12} />
              Stop
            </Button>
          </div>
        )}

        {isWorking && (
          <div className="ml-2 flex items-center gap-1">
            <Button
              size="sm"
              variant="ghost"
              onClick={handlePauseResume}
              className="h-6 px-2 text-[11px]"
            >
              <Pause size={12} />
              Pause
            </Button>
            <Button
              size="sm"
              variant="danger"
              onClick={handleStop}
              className="h-6 px-2 text-[11px]"
            >
              <Square size={12} />
              Stop
            </Button>
          </div>
        )}

        {showInput && (
          <div className="ml-2 flex flex-1 items-center gap-1">
            <input
              type="text"
              value={inputValue}
              onChange={(e) => setInputValue(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") handleSubmitInput();
                if (e.key === "Escape") setShowInput(false);
              }}
              placeholder="Type your input..."
              autoFocus
              className="h-6 flex-1 rounded border border-border bg-bg-tertiary px-2 text-xs text-text-primary placeholder:text-text-tertiary focus:border-accent focus:outline-none"
            />
            <Button size="sm" onClick={handleSubmitInput} className="h-6 px-2 text-[11px]">
              Send
            </Button>
          </div>
        )}

        {/* Right side: cost + context + toggle */}
        <div className="ml-auto flex items-center gap-3">
          <CostTracker
            costUsd={loop.estimated_cost_usd}
            tokensIn={loop.total_tokens_in}
            tokensOut={loop.total_tokens_out}
            compact
          />
          <ContextUsageBar
            used={loop.context_used}
            max={loop.context_max}
            compact
          />
          <IconButton
            icon={expanded ? ChevronUp : ChevronDown}
            tooltip={expanded ? "Collapse overlay (`)" : "Expand overlay (`)"}
            onClick={() => setExpanded((prev) => !prev)}
            className="h-6 w-6"
          />
        </div>
      </div>

      {/* Expandable overlay panel */}
      {expanded && (
        <div
          ref={containerRef}
          className="absolute inset-x-0 top-9 z-10 flex flex-col overflow-hidden border-b border-border"
          style={{
            height: `${heightPct}%`,
            background: "rgb(17 17 19 / 0.92)",
            backdropFilter: "blur(8px)",
          }}
        >
          {/* Tabs */}
          <div className="flex border-b border-white/5">
            {tabs.map((tab) => (
              <button
                key={tab.id}
                onClick={() => setActiveTab(tab.id)}
                className={`flex items-center gap-1.5 px-4 py-1.5 text-xs transition-colors ${
                  activeTab === tab.id
                    ? "border-b-2 border-accent text-text-primary"
                    : "text-text-secondary hover:text-text-primary"
                }`}
              >
                {tab.label}
                <kbd className="rounded bg-bg-tertiary/50 px-1 py-0.5 text-[10px] text-text-tertiary">
                  {tab.shortcut}
                </kbd>
                {tab.count && (
                  <span className="ml-0.5 inline-flex h-4 min-w-[16px] items-center justify-center rounded-full bg-status-warning/20 px-1 text-[10px] font-medium text-status-warning">
                    {tab.count}
                  </span>
                )}
              </button>
            ))}
          </div>

          {/* Tab content */}
          <div className="min-h-0 flex-1 overflow-hidden">
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

          {/* Drag handle */}
          <div
            onMouseDown={handleDragStart}
            className="flex h-1.5 shrink-0 cursor-row-resize items-center justify-center bg-white/5 transition-colors hover:bg-accent/20"
          >
            <div className="h-0.5 w-8 rounded-full bg-text-tertiary/30" />
          </div>
        </div>
      )}
    </>
  );
}
