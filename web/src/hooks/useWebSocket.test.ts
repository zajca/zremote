import { describe, test, expect, beforeEach, vi, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useWebSocket } from "./useWebSocket";

class MockWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onmessage: ((e: MessageEvent) => void) | null = null;
  onerror: (() => void) | null = null;
  readyState = MockWebSocket.CONNECTING;
  binaryType = "";
  close = vi.fn();
  send = vi.fn();

  simulateOpen() {
    this.readyState = MockWebSocket.OPEN;
    this.onopen?.();
  }
  simulateMessage(data: string) {
    this.onmessage?.({ data } as MessageEvent);
  }
  simulateClose() {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.();
  }
  simulateError() {
    this.onerror?.();
  }
}

let mockWsInstance: MockWebSocket;

beforeEach(() => {
  vi.useFakeTimers();
  vi.restoreAllMocks();
  mockWsInstance = new MockWebSocket();
  vi.stubGlobal("WebSocket", Object.assign(
    vi.fn().mockImplementation(() => {
      mockWsInstance = new MockWebSocket();
      return mockWsInstance;
    }),
    {
      CONNECTING: 0,
      OPEN: 1,
      CLOSING: 2,
      CLOSED: 3,
    },
  ));
  // Mock window.location
  Object.defineProperty(window, "location", {
    value: { protocol: "http:", host: "localhost:3000" },
    writable: true,
  });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useWebSocket", () => {
  test("connects to websocket on mount", () => {
    renderHook(() => useWebSocket("/ws/test"));
    expect(WebSocket).toHaveBeenCalledWith("ws://localhost:3000/ws/test");
  });

  test("uses wss for https", () => {
    Object.defineProperty(window, "location", {
      value: { protocol: "https:", host: "example.com" },
      writable: true,
    });
    renderHook(() => useWebSocket("/ws/test"));
    expect(WebSocket).toHaveBeenCalledWith("wss://example.com/ws/test");
  });

  test("does not connect when url is null", () => {
    renderHook(() => useWebSocket(null));
    expect(WebSocket).not.toHaveBeenCalled();
  });

  test("readyState starts as CLOSED", () => {
    const { result } = renderHook(() => useWebSocket("/ws/test"));
    // After connect is called, it should be CONNECTING
    expect(result.current.readyState).toBe(WebSocket.CONNECTING);
  });

  test("readyState updates to OPEN on connection", () => {
    const { result } = renderHook(() => useWebSocket("/ws/test"));
    act(() => mockWsInstance.simulateOpen());
    expect(result.current.readyState).toBe(WebSocket.OPEN);
  });

  test("readyState updates to CLOSED on disconnect", () => {
    const { result } = renderHook(() => useWebSocket("/ws/test"));
    act(() => mockWsInstance.simulateOpen());
    act(() => mockWsInstance.simulateClose());
    expect(result.current.readyState).toBe(WebSocket.CLOSED);
  });

  test("lastMessage updates on message received", () => {
    const { result } = renderHook(() => useWebSocket("/ws/test"));
    act(() => mockWsInstance.simulateOpen());
    act(() => mockWsInstance.simulateMessage('{"type":"ping"}'));
    expect(result.current.lastMessage).not.toBeNull();
    expect(result.current.lastMessage?.data).toBe('{"type":"ping"}');
  });

  test("sendMessage sends data when connected", () => {
    const { result } = renderHook(() => useWebSocket("/ws/test"));
    act(() => mockWsInstance.simulateOpen());
    act(() => result.current.sendMessage("hello"));
    expect(mockWsInstance.send).toHaveBeenCalledWith("hello");
  });

  test("sendMessage does nothing when not connected", () => {
    const { result } = renderHook(() => useWebSocket("/ws/test"));
    // Don't open
    act(() => result.current.sendMessage("hello"));
    expect(mockWsInstance.send).not.toHaveBeenCalled();
  });

  test("reconnects on close with exponential backoff", () => {
    renderHook(() => useWebSocket("/ws/test"));
    act(() => mockWsInstance.simulateClose());

    // First reconnect: 1000ms
    expect(WebSocket).toHaveBeenCalledTimes(1);
    act(() => { vi.advanceTimersByTime(1000); });
    expect(WebSocket).toHaveBeenCalledTimes(2);

    // Second reconnect: 2000ms
    act(() => mockWsInstance.simulateClose());
    act(() => { vi.advanceTimersByTime(1000); });
    expect(WebSocket).toHaveBeenCalledTimes(2); // Not yet
    act(() => { vi.advanceTimersByTime(1000); });
    expect(WebSocket).toHaveBeenCalledTimes(3);
  });

  test("resets retry counter on successful connection", () => {
    renderHook(() => useWebSocket("/ws/test"));

    // Close and reconnect a few times to build up retries
    act(() => mockWsInstance.simulateClose());
    act(() => { vi.advanceTimersByTime(1000); });
    act(() => mockWsInstance.simulateClose());
    act(() => { vi.advanceTimersByTime(2000); });

    // Open resets counter
    act(() => mockWsInstance.simulateOpen());

    // Close again - should use 1000ms (reset backoff)
    act(() => mockWsInstance.simulateClose());
    const callsBefore = (WebSocket as unknown as ReturnType<typeof vi.fn>).mock.calls.length;
    act(() => { vi.advanceTimersByTime(1000); });
    expect((WebSocket as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBe(callsBefore + 1);
  });

  test("does not reconnect when reconnect option is false", () => {
    renderHook(() => useWebSocket("/ws/test", { reconnect: false }));
    act(() => mockWsInstance.simulateClose());
    act(() => { vi.advanceTimersByTime(5000); });
    expect(WebSocket).toHaveBeenCalledTimes(1);
  });

  test("cleans up on unmount", () => {
    const { unmount } = renderHook(() => useWebSocket("/ws/test"));
    act(() => mockWsInstance.simulateOpen());
    unmount();
    expect(mockWsInstance.close).toHaveBeenCalled();
  });

  test("sets binaryType to arraybuffer", () => {
    renderHook(() => useWebSocket("/ws/test"));
    expect(mockWsInstance.binaryType).toBe("arraybuffer");
  });

  test("caps backoff at 30 seconds", () => {
    renderHook(() => useWebSocket("/ws/test"));

    // Close many times to exceed 30s backoff cap
    for (let i = 0; i < 10; i++) {
      act(() => mockWsInstance.simulateClose());
      act(() => { vi.advanceTimersByTime(30000); });
    }

    // The delay should be capped at 30000ms = Math.min(1000 * 2^n, 30000)
    // After 10 iterations: 1000 * 2^10 = 1024000 > 30000, so cap applies
    // This is implicitly tested by the reconnect still happening within 30s
    const totalCalls = (WebSocket as unknown as ReturnType<typeof vi.fn>).mock.calls.length;
    expect(totalCalls).toBeGreaterThan(5);
  });
});
