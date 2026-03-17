import { useEffect, useRef } from "react";
import { useAgenticStore } from "../stores/agentic-store";
import { useClaudeTaskStore } from "../stores/claude-task-store";
import {
  dispatchWsDisconnected,
  dispatchWsReconnected,
} from "../components/layout/ReconnectBanner";
import { showToast } from "../components/layout/Toast";
import type {
  AgenticLoop,
  ToolCall,
  TranscriptEntry,
} from "../types/agentic";

interface EventHandler {
  onHostUpdate?: () => void;
  onSessionUpdate?: () => void;
  onProjectUpdate?: () => void;
}

interface ServerEvent {
  type: string;
  loop?: AgenticLoop;
  tool_call?: ToolCall;
  transcript_entry?: TranscriptEntry;
  loop_id?: string;
  host_id?: string;
  project_path?: string;
  message?: string;
  // Claude task event fields
  task_id?: string;
  session_id?: string;
  status?: string;
  summary?: string;
  total_cost_usd?: number;
}

const RECONNECT_DELAY_MS = 3000;

export function useRealtimeUpdates(handlers: EventHandler) {
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  useEffect(() => {
    let ws: WebSocket | null = null;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    let disposed = false;

    function connect() {
      if (disposed) return;

      const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
      const url = `${protocol}//${window.location.host}/ws/events`;
      ws = new WebSocket(url);

      ws.onopen = () => {
        dispatchWsReconnected();
      };

      ws.onmessage = (event: MessageEvent) => {
        let parsed: ServerEvent;
        try {
          parsed = JSON.parse(event.data as string) as ServerEvent;
        } catch {
          return;
        }

        const store = useAgenticStore.getState();
        const taskStore = useClaudeTaskStore.getState();

        switch (parsed.type) {
          case "host_connected":
          case "host_disconnected":
          case "host_status_changed":
            handlersRef.current.onHostUpdate?.();
            break;
          case "session_created":
          case "session_closed":
            handlersRef.current.onSessionUpdate?.();
            break;
          case "projects_updated":
            handlersRef.current.onProjectUpdate?.();
            break;
          case "lagged":
            handlersRef.current.onHostUpdate?.();
            handlersRef.current.onSessionUpdate?.();
            handlersRef.current.onProjectUpdate?.();
            // Also refresh agentic loops on lag
            window.dispatchEvent(new Event("zremote:agentic-loop-update"));
            break;
          case "agentic_loop_detected":
          case "agentic_loop_state_update":
            if (parsed.loop) {
              store.updateLoop(parsed.loop);
              window.dispatchEvent(
                new Event("zremote:agentic-loop-update"),
              );
            }
            break;
          case "agentic_loop_ended":
            if (parsed.loop) {
              store.updateLoop(parsed.loop);
              window.dispatchEvent(
                new Event("zremote:agentic-loop-update"),
              );
            }
            break;
          case "agentic_loop_tool_call":
            if (parsed.tool_call && parsed.loop_id) {
              store.addToolCall(parsed.loop_id, parsed.tool_call);
            }
            break;
          case "agentic_loop_tool_result":
            if (parsed.tool_call && parsed.loop_id) {
              store.updateToolCall(parsed.loop_id, parsed.tool_call);
            }
            break;
          case "agentic_loop_transcript":
            if (parsed.transcript_entry && parsed.loop_id) {
              store.addTranscript(parsed.loop_id, parsed.transcript_entry);
            }
            break;
          case "agentic_loop_metrics":
            if (parsed.loop) {
              store.updateLoop(parsed.loop);
            }
            break;
          case "worktree_error":
            if (parsed.message) {
              showToast(`Worktree error: ${parsed.message}`, "error");
            }
            break;
          case "claude_task_started":
            if (parsed.task_id && parsed.session_id && parsed.host_id && parsed.project_path) {
              taskStore.handleTaskStarted({
                task_id: parsed.task_id,
                session_id: parsed.session_id,
                host_id: parsed.host_id,
                project_path: parsed.project_path,
              });
            }
            break;
          case "claude_task_updated":
            if (parsed.task_id && parsed.status) {
              taskStore.handleTaskUpdated({
                task_id: parsed.task_id,
                status: parsed.status,
                loop_id: parsed.loop_id ?? null,
              });
            }
            break;
          case "claude_task_ended":
            if (parsed.task_id && parsed.status) {
              taskStore.handleTaskEnded({
                task_id: parsed.task_id,
                status: parsed.status,
                summary: parsed.summary ?? null,
                total_cost_usd: parsed.total_cost_usd ?? 0,
              });
            }
            break;
        }
      };

      ws.onclose = () => {
        if (!disposed) {
          dispatchWsDisconnected();
          reconnectTimer = setTimeout(connect, RECONNECT_DELAY_MS);
        }
      };

      ws.onerror = () => {
        // onclose will fire after onerror, triggering reconnect
      };
    }

    connect();

    return () => {
      disposed = true;
      if (reconnectTimer) clearTimeout(reconnectTimer);
      if (ws) {
        ws.onclose = null;
        ws.close();
      }
    };
  }, []);
}
