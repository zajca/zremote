import "@xterm/xterm/css/xterm.css";

import { useEffect, useRef, useCallback } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";

interface TerminalProps {
  sessionId: string;
}

interface WsMessage {
  type: "output" | "session_closed" | "session_suspended" | "session_resumed" | "error" | "scrollback_start" | "scrollback_end";
  data?: string;
  exit_code?: number | null;
  message?: string;
}

export function Terminal({ sessionId }: TerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<XTerm | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const closedRef = useRef(false);
  const suspendedRef = useRef(false);
  const retriesRef = useRef(0);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const unmountedRef = useRef(false);
  // RAF-based write batching: accumulate chunks, flush once per frame
  const writeBufferRef = useRef<Uint8Array[]>([]);
  const rafIdRef = useRef<number | null>(null);

  const flushWrites = useCallback(() => {
    rafIdRef.current = null;
    const term = termRef.current;
    const chunks = writeBufferRef.current;
    if (!term || chunks.length === 0) return;

    // Concatenate all buffered chunks into a single write
    if (chunks.length === 1) {
      term.write(chunks[0]!);
    } else {
      let totalLen = 0;
      for (const c of chunks) totalLen += c.length;
      const merged = new Uint8Array(totalLen);
      let offset = 0;
      for (const c of chunks) {
        merged.set(c, offset);
        offset += c.length;
      }
      term.write(merged);
    }
    writeBufferRef.current = [];
  }, []);

  const scheduleWrite = useCallback(
    (bytes: Uint8Array) => {
      writeBufferRef.current.push(bytes);
      if (rafIdRef.current === null) {
        rafIdRef.current = requestAnimationFrame(flushWrites);
      }
    },
    [flushWrites],
  );

  const handleWsMessage = useCallback(
    (event: MessageEvent) => {
      const term = termRef.current;
      if (!term) return;

      let msg: WsMessage;
      try {
        msg = JSON.parse(
          typeof event.data === "string"
            ? event.data
            : new TextDecoder().decode(event.data as ArrayBuffer),
        ) as WsMessage;
      } catch {
        console.error("failed to parse terminal WebSocket message");
        return;
      }

      if (msg.type === "scrollback_start") {
        // Cancel pending RAF and clear write buffer to avoid stale data
        if (rafIdRef.current !== null) {
          cancelAnimationFrame(rafIdRef.current);
          rafIdRef.current = null;
        }
        writeBufferRef.current = [];
        // Full reset: clears buffer + resets ANSI parser state
        term.reset();
        return;
      } else if (msg.type === "scrollback_end") {
        // Marker for future use
        return;
      } else if (msg.type === "output" && msg.data) {
        const bytes = Uint8Array.from(atob(msg.data), (c) => c.charCodeAt(0));
        scheduleWrite(bytes);
      } else if (msg.type === "session_closed") {
        closedRef.current = true;
        term.write(
          `\r\n\x1b[90m[Session closed${msg.exit_code != null ? ` (exit code: ${String(msg.exit_code)})` : ""}]\x1b[0m`,
        );
      } else if (msg.type === "session_suspended") {
        suspendedRef.current = true;
        term.write(
          `\r\n\x1b[33m[Session suspended - waiting for agent reconnection...]\x1b[0m`,
        );
      } else if (msg.type === "session_resumed") {
        suspendedRef.current = false;
        term.write(
          `\r\n\x1b[32m[Session resumed]\x1b[0m\r\n`,
        );
      } else if (msg.type === "error" && msg.message) {
        term.write(`\r\n\x1b[31m[Error: ${msg.message}]\x1b[0m`);
      }
    },
    [scheduleWrite],
  );

  // Connect WebSocket directly (bypass React state)
  const connect = useCallback(() => {
    if (unmountedRef.current) return;

    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    const wsUrl = `${protocol}//${window.location.host}/ws/terminal/${sessionId}`;
    const ws = new WebSocket(wsUrl);
    ws.binaryType = "arraybuffer";
    wsRef.current = ws;

    ws.onopen = () => {
      if (unmountedRef.current) {
        ws.close();
        return;
      }
      retriesRef.current = 0;
      // Focus terminal on connect
      termRef.current?.focus();
      // Send initial resize so the server knows our dimensions
      const term = termRef.current;
      if (term) {
        ws.send(
          JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }),
        );
      }
    };

    ws.onmessage = handleWsMessage;

    ws.onclose = () => {
      if (unmountedRef.current) return;
      wsRef.current = null;

      const delay = Math.min(1000 * 2 ** retriesRef.current, 30000);
      retriesRef.current += 1;
      reconnectTimerRef.current = setTimeout(() => {
        if (!unmountedRef.current) {
          connect();
        }
      }, delay);
    };

    ws.onerror = () => {
      // onclose fires after onerror
    };
  }, [sessionId, handleWsMessage]);

  // Initialize xterm.js + WebSocket
  useEffect(() => {
    if (!containerRef.current) return;
    unmountedRef.current = false;
    suspendedRef.current = false;

    const term = new XTerm({
      theme: {
        background: "#0a0a0b",
        foreground: "#f0f0f3",
        cursor: "#f0f0f3",
        selectionBackground: "#5e6ad240",
      },
      fontFamily: "'JetBrains Mono Variable', 'JetBrainsMono Nerd Font', 'Symbols Nerd Font', monospace",
      fontSize: 14,
      cursorBlink: true,
      convertEol: true,
    });

    const fitAddon = new FitAddon();
    const webLinksAddon = new WebLinksAddon();

    term.loadAddon(fitAddon);
    term.loadAddon(webLinksAddon);
    term.open(containerRef.current);

    // Try WebGL renderer, fall back to canvas
    try {
      const webglAddon = new WebglAddon();
      webglAddon.onContextLoss(() => webglAddon.dispose());
      term.loadAddon(webglAddon);
    } catch {
      // WebGL not available, canvas renderer is fine
    }

    termRef.current = term;
    fitAddonRef.current = fitAddon;

    // Initial fit
    requestAnimationFrame(() => {
      fitAddon.fit();
    });

    // User input -> send directly via WS ref
    const inputDisposable = term.onData((data) => {
      if (!closedRef.current && !suspendedRef.current && wsRef.current?.readyState === WebSocket.OPEN) {
        wsRef.current.send(JSON.stringify({ type: "input", data }));
      }
    });

    // Resize handling
    let resizeTimer: ReturnType<typeof setTimeout> | null = null;
    const observer = new ResizeObserver(() => {
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        fitAddon.fit();
        if (wsRef.current?.readyState === WebSocket.OPEN) {
          wsRef.current.send(
            JSON.stringify({
              type: "resize",
              cols: term.cols,
              rows: term.rows,
            }),
          );
        }
      }, 150);
    });
    observer.observe(containerRef.current);

    // Start WebSocket connection
    connect();

    return () => {
      unmountedRef.current = true;
      observer.disconnect();
      if (resizeTimer) clearTimeout(resizeTimer);
      inputDisposable.dispose();

      if (reconnectTimerRef.current !== null) {
        clearTimeout(reconnectTimerRef.current);
        reconnectTimerRef.current = null;
      }
      if (wsRef.current) {
        wsRef.current.onclose = null;
        wsRef.current.close();
        wsRef.current = null;
      }
      if (rafIdRef.current !== null) {
        cancelAnimationFrame(rafIdRef.current);
        rafIdRef.current = null;
      }
      writeBufferRef.current = [];

      term.dispose();
      termRef.current = null;
      fitAddonRef.current = null;
    };
  }, [connect]);

  return (
    <div
      ref={containerRef}
      className="h-full w-full"
      style={{ backgroundColor: "#0a0a0b" }}
    />
  );
}
