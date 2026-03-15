import {
  Check,
  MessageSquare,
  Pause,
  Play,
  Square,
  X,
} from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import type { AgenticStatus, UserAction } from "../../types/agentic";
import { Button } from "../ui/Button";

interface AgenticActionBarProps {
  status: AgenticStatus;
  onAction: (action: UserAction, payload?: string) => void;
}

export function AgenticActionBar({ status, onAction }: AgenticActionBarProps) {
  const [pendingAction, setPendingAction] = useState<UserAction | null>(null);
  const [showInput, setShowInput] = useState(false);
  const [inputValue, setInputValue] = useState("");

  const isWaiting = status === "WaitingForInput";
  const isWorking = status === "Working";
  const isPaused = status === "Paused";
  const isActive = isWaiting || isWorking || isPaused;

  const handleAction = useCallback(
    (action: UserAction, payload?: string) => {
      setPendingAction(action);
      onAction(action, payload);
      // Clear optimistic state after short delay
      setTimeout(() => setPendingAction(null), 500);
    },
    [onAction],
  );

  const handleApprove = useCallback(() => {
    if (isWaiting) handleAction("Approve");
  }, [isWaiting, handleAction]);

  const handleReject = useCallback(() => {
    if (isWaiting) handleAction("Reject");
  }, [isWaiting, handleAction]);

  const handleProvideInput = useCallback(() => {
    setShowInput(true);
    setInputValue("");
  }, []);

  const handleSubmitInput = useCallback(() => {
    if (inputValue.trim()) {
      handleAction("ProvideInput", inputValue.trim());
      setShowInput(false);
      setInputValue("");
    }
  }, [inputValue, handleAction]);

  const handlePauseResume = useCallback(() => {
    if (isWorking) handleAction("Pause");
    else if (isPaused) handleAction("Resume");
  }, [isWorking, isPaused, handleAction]);

  const handleStop = useCallback(() => {
    if (isActive && window.confirm("Stop this agentic loop?")) {
      handleAction("Stop");
    }
  }, [isActive, handleAction]);

  // Keyboard shortcuts
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      // Don't capture when typing in an input
      if (
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement
      ) {
        return;
      }

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
      }
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [
    isWaiting,
    isWorking,
    isPaused,
    isActive,
    showInput,
    handleApprove,
    handleReject,
    handleProvideInput,
    handlePauseResume,
    handleStop,
  ]);

  return (
    <div className="flex items-center gap-2 border-b border-border px-3 py-2">
      <Button
        size="sm"
        variant="primary"
        disabled={!isWaiting || pendingAction === "Approve"}
        onClick={handleApprove}
        className="bg-status-online hover:bg-status-online/80"
      >
        <Check size={14} />
        {pendingAction === "Approve" ? "Approving..." : "Approve"}
      </Button>

      <Button
        size="sm"
        variant="danger"
        disabled={!isWaiting || pendingAction === "Reject"}
        onClick={handleReject}
      >
        <X size={14} />
        {pendingAction === "Reject" ? "Rejecting..." : "Reject"}
      </Button>

      <Button
        size="sm"
        variant="secondary"
        disabled={!isWaiting}
        onClick={handleProvideInput}
      >
        <MessageSquare size={14} />
        Input
      </Button>

      <div className="mx-1 h-4 w-px bg-border" />

      <Button
        size="sm"
        variant="ghost"
        disabled={!isWorking && !isPaused}
        onClick={handlePauseResume}
      >
        {isPaused ? <Play size={14} /> : <Pause size={14} />}
        {isPaused ? "Resume" : "Pause"}
      </Button>

      <Button
        size="sm"
        variant="danger"
        disabled={!isActive}
        onClick={handleStop}
      >
        <Square size={14} />
        Stop
      </Button>

      {showInput && (
        <div className="ml-2 flex flex-1 items-center gap-2">
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
            className="h-7 flex-1 rounded-md border border-border bg-bg-tertiary px-2 text-sm text-text-primary placeholder:text-text-tertiary focus:border-accent focus:outline-none"
          />
          <Button size="sm" onClick={handleSubmitInput}>
            Send
          </Button>
        </div>
      )}
    </div>
  );
}
