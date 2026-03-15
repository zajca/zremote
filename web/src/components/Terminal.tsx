import "@xterm/xterm/css/xterm.css";

import { useEffect, useRef } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { useWebSocket } from "../hooks/useWebSocket";

interface TerminalProps {
  sessionId: string;
}

interface WsMessage {
  type: "output" | "session_closed" | "error";
  data?: number[] | string;
  exit_code?: number | null;
  message?: string;
}

export function Terminal({ sessionId }: TerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<XTerm | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const closedRef = useRef(false);

  const { sendMessage, lastMessage, readyState } = useWebSocket(
    `/ws/terminal/${sessionId}`,
  );

  // Initialize xterm.js
  useEffect(() => {
    if (!containerRef.current) return;

    const term = new XTerm({
      theme: {
        background: "#0a0a0b",
        foreground: "#f0f0f3",
        cursor: "#f0f0f3",
        selectionBackground: "#5e6ad240",
      },
      fontFamily: "'JetBrains Mono Variable', monospace",
      fontSize: 14,
      cursorBlink: true,
      convertEol: true,
    });

    const fitAddon = new FitAddon();
    const webLinksAddon = new WebLinksAddon();

    term.loadAddon(fitAddon);
    term.loadAddon(webLinksAddon);
    term.open(containerRef.current);

    termRef.current = term;
    fitAddonRef.current = fitAddon;

    // Initial fit
    requestAnimationFrame(() => {
      fitAddon.fit();
    });

    return () => {
      term.dispose();
      termRef.current = null;
      fitAddonRef.current = null;
    };
  }, []);

  // Handle user input -> send to WS
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;

    const disposable = term.onData((data) => {
      if (!closedRef.current) {
        sendMessage(JSON.stringify({ type: "input", data }));
      }
    });

    return () => {
      disposable.dispose();
    };
  }, [sendMessage]);

  // Handle incoming WS messages
  useEffect(() => {
    if (!lastMessage || !termRef.current) return;

    let msg: WsMessage;
    try {
      msg = JSON.parse(
        typeof lastMessage.data === "string"
          ? lastMessage.data
          : new TextDecoder().decode(lastMessage.data as ArrayBuffer),
      ) as WsMessage;
    } catch {
      console.error("failed to parse terminal WebSocket message");
      return;
    }

    if (msg.type === "output" && msg.data) {
      // Server sends Vec<u8> which serializes as a JSON number array
      const payload = Array.isArray(msg.data)
        ? new Uint8Array(msg.data)
        : msg.data;
      termRef.current.write(payload);
    } else if (msg.type === "session_closed") {
      closedRef.current = true;
      termRef.current.write(
        `\r\n\x1b[90m[Session closed${msg.exit_code != null ? ` (exit code: ${String(msg.exit_code)})` : ""}]\x1b[0m`,
      );
    } else if (msg.type === "error" && msg.message) {
      termRef.current.write(`\r\n\x1b[31m[Error: ${msg.message}]\x1b[0m`);
    }
  }, [lastMessage]);

  // Send resize events to server
  useEffect(() => {
    const fitAddon = fitAddonRef.current;
    const term = termRef.current;
    const container = containerRef.current;
    if (!fitAddon || !term || !container) return;

    let resizeTimer: ReturnType<typeof setTimeout> | null = null;

    const observer = new ResizeObserver(() => {
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        fitAddon.fit();
        if (readyState === WebSocket.OPEN) {
          sendMessage(
            JSON.stringify({
              type: "resize",
              cols: term.cols,
              rows: term.rows,
            }),
          );
        }
      }, 150);
    });

    observer.observe(container);

    return () => {
      observer.disconnect();
      if (resizeTimer) clearTimeout(resizeTimer);
    };
  }, [sendMessage, readyState]);

  // Focus terminal when WS connects
  useEffect(() => {
    if (readyState === WebSocket.OPEN && termRef.current) {
      termRef.current.focus();
    }
  }, [readyState]);

  return (
    <div
      ref={containerRef}
      className="h-full w-full"
      style={{ backgroundColor: "#0a0a0b" }}
    />
  );
}
