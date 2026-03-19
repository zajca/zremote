import { useCallback, useEffect, useRef, useState } from "react";

interface UseWebSocketOptions {
  reconnect?: boolean;
}

interface UseWebSocketReturn {
  sendMessage: (data: string | ArrayBuffer) => void;
  lastMessage: MessageEvent | null;
  readyState: number;
}

export function useWebSocket(
  url: string | null,
  options?: UseWebSocketOptions,
): UseWebSocketReturn {
  const { reconnect = true } = options ?? {};
  const [lastMessage, setLastMessage] = useState<MessageEvent | null>(null);
  const [readyState, setReadyState] = useState<number>(WebSocket.CLOSED);
  const wsRef = useRef<WebSocket | null>(null);
  const retriesRef = useRef(0);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const unmountedRef = useRef(false);

  const clearReconnectTimer = useCallback(() => {
    if (reconnectTimerRef.current !== null) {
      clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
  }, []);

  const connect = useCallback(() => {
    if (!url || unmountedRef.current) return;

    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    const wsUrl = `${protocol}//${window.location.host}${url}`;
    const ws = new WebSocket(wsUrl);
    ws.binaryType = "arraybuffer";
    wsRef.current = ws;

    ws.onopen = () => {
      if (unmountedRef.current) {
        ws.close();
        return;
      }
      retriesRef.current = 0;
      setReadyState(WebSocket.OPEN);
    };

    ws.onmessage = (event: MessageEvent) => {
      if (!unmountedRef.current) {
        setLastMessage(event);
      }
    };

    ws.onclose = () => {
      if (unmountedRef.current) return;
      setReadyState(WebSocket.CLOSED);
      wsRef.current = null;

      if (reconnect) {
        const delay = Math.min(1000 * 2 ** retriesRef.current, 30000);
        retriesRef.current += 1;
        reconnectTimerRef.current = setTimeout(() => {
          if (!unmountedRef.current) {
            connect();
          }
        }, delay);
      }
    };

    ws.onerror = () => {
      // onclose will fire after onerror
    };

    setReadyState(WebSocket.CONNECTING);
  }, [url, reconnect]);

  useEffect(() => {
    unmountedRef.current = false;
    connect();

    return () => {
      unmountedRef.current = true;
      clearReconnectTimer();
      if (wsRef.current) {
        wsRef.current.onclose = null;
        wsRef.current.close();
        wsRef.current = null;
      }
    };
  }, [connect, clearReconnectTimer]);

  const sendMessage = useCallback((data: string | ArrayBuffer) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(data);
    }
  }, []);

  return { sendMessage, lastMessage, readyState };
}
