import { useEffect, useRef } from "react";

interface EventHandler {
  onHostUpdate?: () => void;
  onSessionUpdate?: () => void;
}

interface ServerEvent {
  type: string;
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
            // Server says we missed events, refetch everything
            handlersRef.current.onHostUpdate?.();
            handlersRef.current.onSessionUpdate?.();
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
