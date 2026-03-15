import { useEffect, useRef } from "react";
import { useAgenticStore } from "../stores/agentic-store";
import type {
  AgenticLoop,
  ToolCall,
  TranscriptEntry,
} from "../types/agentic";

interface EventHandler {
  onHostUpdate?: () => void;
  onSessionUpdate?: () => void;
}

interface ServerEvent {
  type: string;
  loop?: AgenticLoop;
  tool_call?: ToolCall;
  transcript_entry?: TranscriptEntry;
  loop_id?: string;
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

      ws.onmessage = (event: MessageEvent) => {
        let parsed: ServerEvent;
        try {
          parsed = JSON.parse(event.data as string) as ServerEvent;
        } catch {
          return;
        }

        const store = useAgenticStore.getState();

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
          case "lagged":
            handlersRef.current.onHostUpdate?.();
            handlersRef.current.onSessionUpdate?.();
            break;
          case "agentic_loop_detected":
          case "agentic_loop_state_update":
            if (parsed.loop) {
              store.updateLoop(parsed.loop);
              window.dispatchEvent(
                new Event("myremote:agentic-loop-update"),
              );
            }
            break;
          case "agentic_loop_ended":
            if (parsed.loop) {
              store.updateLoop(parsed.loop);
              window.dispatchEvent(
                new Event("myremote:agentic-loop-update"),
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
        }
      };

      ws.onclose = () => {
        if (!disposed) {
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
