import { describe, test, expect, beforeEach, vi, afterEach } from "vitest";
import { renderHook } from "@testing-library/react";

// Mock modules before imports
vi.mock("../stores/agentic-store", () => ({
  useAgenticStore: {
    getState: vi.fn(() => ({
      updateLoop: vi.fn(),
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    })),
  },
}));

vi.mock("../stores/claude-task-store", () => ({
  useClaudeTaskStore: {
    getState: vi.fn(() => ({
      handleTaskStarted: vi.fn(),
      handleTaskUpdated: vi.fn(),
      handleTaskEnded: vi.fn(),
    })),
  },
}));

vi.mock("../components/layout/ReconnectBanner", () => ({
  dispatchWsReconnected: vi.fn(),
  dispatchWsDisconnected: vi.fn(),
}));

vi.mock("../components/layout/Toast", () => ({
  showToast: vi.fn(),
}));

vi.mock("../stores/notification-store", () => ({
  useNotificationStore: {
    getState: vi.fn(() => ({
      addOrUpdate: vi.fn(),
      handleLoopResolved: vi.fn(),
      handleToolResolved: vi.fn(),
      notifications: new Map(),
      browserEnabled: false,
    })),
  },
}));

vi.mock("../lib/browser-notifications", () => ({
  showBrowserNotification: vi.fn(),
}));

import { useRealtimeUpdates } from "./useRealtimeUpdates";
import { useAgenticStore } from "../stores/agentic-store";
import { useClaudeTaskStore } from "../stores/claude-task-store";
import { useNotificationStore } from "../stores/notification-store";
import { dispatchWsReconnected, dispatchWsDisconnected } from "../components/layout/ReconnectBanner";
import { showToast } from "../components/layout/Toast";
import { showBrowserNotification } from "../lib/browser-notifications";

class MockWebSocket {
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  close = vi.fn();

  simulateOpen() { this.onopen?.(); }
  simulateMessage(data: unknown) { this.onmessage?.({ data: JSON.stringify(data) }); }
  simulateClose() { this.onclose?.(); }
}

let mockWs: MockWebSocket;

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  mockWs = new MockWebSocket();
  vi.stubGlobal("WebSocket", vi.fn().mockImplementation(() => {
    mockWs = new MockWebSocket();
    return mockWs;
  }));
  Object.defineProperty(window, "location", {
    value: { protocol: "http:", host: "localhost:3000" },
    writable: true,
  });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useRealtimeUpdates", () => {
  test("connects to /ws/events on mount", () => {
    renderHook(() => useRealtimeUpdates({}));
    expect(WebSocket).toHaveBeenCalledWith("ws://localhost:3000/ws/events");
  });

  test("dispatches reconnected on open", () => {
    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateOpen();
    expect(dispatchWsReconnected).toHaveBeenCalled();
  });

  test("dispatches disconnected and reconnects on close", () => {
    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateClose();
    expect(dispatchWsDisconnected).toHaveBeenCalled();

    // Should reconnect after 3 seconds
    vi.advanceTimersByTime(3000);
    expect(WebSocket).toHaveBeenCalledTimes(2);
  });

  test("cleans up on unmount", () => {
    const { unmount } = renderHook(() => useRealtimeUpdates({}));
    unmount();
    expect(mockWs.close).toHaveBeenCalled();
  });

  test("calls onHostUpdate for host events", () => {
    const onHostUpdate = vi.fn();
    renderHook(() => useRealtimeUpdates({ onHostUpdate }));
    mockWs.simulateMessage({ type: "host_connected" });
    expect(onHostUpdate).toHaveBeenCalled();
  });

  test("calls onHostUpdate for host_disconnected", () => {
    const onHostUpdate = vi.fn();
    renderHook(() => useRealtimeUpdates({ onHostUpdate }));
    mockWs.simulateMessage({ type: "host_disconnected" });
    expect(onHostUpdate).toHaveBeenCalled();
  });

  test("calls onHostUpdate for host_status_changed", () => {
    const onHostUpdate = vi.fn();
    renderHook(() => useRealtimeUpdates({ onHostUpdate }));
    mockWs.simulateMessage({ type: "host_status_changed" });
    expect(onHostUpdate).toHaveBeenCalled();
  });

  test("calls onSessionUpdate for session events", () => {
    const onSessionUpdate = vi.fn();
    renderHook(() => useRealtimeUpdates({ onSessionUpdate }));
    mockWs.simulateMessage({ type: "session_created" });
    expect(onSessionUpdate).toHaveBeenCalled();
  });

  test("calls onSessionUpdate for session_closed", () => {
    const onSessionUpdate = vi.fn();
    renderHook(() => useRealtimeUpdates({ onSessionUpdate }));
    mockWs.simulateMessage({ type: "session_closed" });
    expect(onSessionUpdate).toHaveBeenCalled();
  });

  test("calls onProjectUpdate for projects_updated", () => {
    const onProjectUpdate = vi.fn();
    renderHook(() => useRealtimeUpdates({ onProjectUpdate }));
    mockWs.simulateMessage({ type: "projects_updated" });
    expect(onProjectUpdate).toHaveBeenCalled();
  });

  test("calls all handlers on lagged event", () => {
    const onHostUpdate = vi.fn();
    const onSessionUpdate = vi.fn();
    const onProjectUpdate = vi.fn();
    renderHook(() => useRealtimeUpdates({ onHostUpdate, onSessionUpdate, onProjectUpdate }));
    mockWs.simulateMessage({ type: "lagged" });
    expect(onHostUpdate).toHaveBeenCalled();
    expect(onSessionUpdate).toHaveBeenCalled();
    expect(onProjectUpdate).toHaveBeenCalled();
  });

  test("updates agentic store on loop detected", () => {
    const mockUpdateLoop = vi.fn();
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: mockUpdateLoop,
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    const loop = { id: "l1", status: "working" };
    mockWs.simulateMessage({ type: "agentic_loop_detected", loop });
    expect(mockUpdateLoop).toHaveBeenCalledWith(loop);
  });

  test("updates agentic store on loop state update", () => {
    const mockUpdateLoop = vi.fn();
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: mockUpdateLoop,
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    const loop = { id: "l1", status: "completed" };
    mockWs.simulateMessage({ type: "agentic_loop_state_update", loop });
    expect(mockUpdateLoop).toHaveBeenCalledWith(loop);
  });

  test("updates agentic store on loop ended", () => {
    const mockUpdateLoop = vi.fn();
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: mockUpdateLoop,
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    const loop = { id: "l1", status: "completed" };
    mockWs.simulateMessage({ type: "agentic_loop_ended", loop });
    expect(mockUpdateLoop).toHaveBeenCalledWith(loop);
  });

  test("adds tool call to store on agentic_loop_tool_call", () => {
    const mockAddToolCall = vi.fn();
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: vi.fn(),
      addToolCall: mockAddToolCall,
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    const toolCall = { id: "tc1", tool_name: "Read" };
    mockWs.simulateMessage({ type: "agentic_loop_tool_call", loop_id: "l1", tool_call: toolCall });
    expect(mockAddToolCall).toHaveBeenCalledWith("l1", toolCall);
  });

  test("updates tool call on agentic_loop_tool_result", () => {
    const mockUpdateToolCall = vi.fn();
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: vi.fn(),
      addToolCall: vi.fn(),
      updateToolCall: mockUpdateToolCall,
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    const toolCall = { id: "tc1", tool_name: "Read", status: "completed" };
    mockWs.simulateMessage({ type: "agentic_loop_tool_result", loop_id: "l1", tool_call: toolCall });
    expect(mockUpdateToolCall).toHaveBeenCalledWith("l1", toolCall);
  });

  test("adds transcript entry on agentic_loop_transcript", () => {
    const mockAddTranscript = vi.fn();
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: vi.fn(),
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: mockAddTranscript,
    });

    renderHook(() => useRealtimeUpdates({}));
    const entry = { id: 1, role: "assistant", content: "Hello" };
    mockWs.simulateMessage({ type: "agentic_loop_transcript", loop_id: "l1", transcript_entry: entry });
    expect(mockAddTranscript).toHaveBeenCalledWith("l1", entry);
  });

  test("updates loop on agentic_loop_metrics", () => {
    const mockUpdateLoop = vi.fn();
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: mockUpdateLoop,
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    const loop = { id: "l1", total_tokens_in: 500 };
    mockWs.simulateMessage({ type: "agentic_loop_metrics", loop });
    expect(mockUpdateLoop).toHaveBeenCalledWith(loop);
  });

  test("shows toast on worktree_error", () => {
    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({ type: "worktree_error", message: "branch conflict" });
    expect(showToast).toHaveBeenCalledWith("Worktree error: branch conflict", "error");
  });

  test("does not show toast for worktree_error without message", () => {
    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({ type: "worktree_error" });
    expect(showToast).not.toHaveBeenCalled();
  });

  test("handles claude_task_started event", () => {
    const mockHandleStarted = vi.fn();
    (useClaudeTaskStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      handleTaskStarted: mockHandleStarted,
      handleTaskUpdated: vi.fn(),
      handleTaskEnded: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "claude_task_started",
      task_id: "t1",
      session_id: "s1",
      host_id: "h1",
      project_path: "/app",
    });
    expect(mockHandleStarted).toHaveBeenCalledWith({
      task_id: "t1",
      session_id: "s1",
      host_id: "h1",
      project_path: "/app",
    });
  });

  test("handles claude_task_updated event", () => {
    const mockHandleUpdated = vi.fn();
    (useClaudeTaskStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      handleTaskStarted: vi.fn(),
      handleTaskUpdated: mockHandleUpdated,
      handleTaskEnded: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "claude_task_updated",
      task_id: "t1",
      status: "active",
      loop_id: "l1",
    });
    expect(mockHandleUpdated).toHaveBeenCalledWith({
      task_id: "t1",
      status: "active",
      loop_id: "l1",
    });
  });

  test("handles claude_task_ended event", () => {
    const mockHandleEnded = vi.fn();
    (useClaudeTaskStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      handleTaskStarted: vi.fn(),
      handleTaskUpdated: vi.fn(),
      handleTaskEnded: mockHandleEnded,
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "claude_task_ended",
      task_id: "t1",
      status: "completed",
      summary: "Done",
      total_cost_usd: 1.5,
    });
    expect(mockHandleEnded).toHaveBeenCalledWith({
      task_id: "t1",
      status: "completed",
      summary: "Done",
      total_cost_usd: 1.5,
    });
  });

  test("handles claude_task_ended with null summary defaults", () => {
    const mockHandleEnded = vi.fn();
    (useClaudeTaskStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      handleTaskStarted: vi.fn(),
      handleTaskUpdated: vi.fn(),
      handleTaskEnded: mockHandleEnded,
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "claude_task_ended",
      task_id: "t1",
      status: "error",
    });
    expect(mockHandleEnded).toHaveBeenCalledWith({
      task_id: "t1",
      status: "error",
      summary: null,
      total_cost_usd: 0,
    });
  });

  test("ignores invalid JSON messages", () => {
    renderHook(() => useRealtimeUpdates({}));
    // Directly invoke with invalid JSON
    mockWs.onmessage?.({ data: "not json" });
    // Should not throw
  });

  test("does not call handlers for missing fields", () => {
    const mockAddToolCall = vi.fn();
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: vi.fn(),
      addToolCall: mockAddToolCall,
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    // tool_call event without tool_call field
    mockWs.simulateMessage({ type: "agentic_loop_tool_call", loop_id: "l1" });
    expect(mockAddToolCall).not.toHaveBeenCalled();
  });

  test("does not call task handlers for missing fields", () => {
    const mockHandleStarted = vi.fn();
    (useClaudeTaskStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      handleTaskStarted: mockHandleStarted,
      handleTaskUpdated: vi.fn(),
      handleTaskEnded: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    // Missing session_id
    mockWs.simulateMessage({ type: "claude_task_started", task_id: "t1" });
    expect(mockHandleStarted).not.toHaveBeenCalled();
  });

  test("shows toast on host_connected", () => {
    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({ type: "host_connected", hostname: "my-host" });
    expect(showToast).toHaveBeenCalledWith("Host my-host connected", "success");
  });

  test("shows toast on host_disconnected", () => {
    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({ type: "host_disconnected", hostname: "my-host" });
    expect(showToast).toHaveBeenCalledWith("Host my-host disconnected", "error");
  });

  test("shows toast on session_suspended", () => {
    const onSessionUpdate = vi.fn();
    renderHook(() => useRealtimeUpdates({ onSessionUpdate }));
    mockWs.simulateMessage({ type: "session_suspended" });
    expect(onSessionUpdate).toHaveBeenCalled();
    expect(showToast).toHaveBeenCalledWith("Session suspended - agent reconnecting", "info");
  });

  test("shows toast on session_resumed", () => {
    const onSessionUpdate = vi.fn();
    renderHook(() => useRealtimeUpdates({ onSessionUpdate }));
    mockWs.simulateMessage({ type: "session_resumed" });
    expect(onSessionUpdate).toHaveBeenCalled();
    expect(showToast).toHaveBeenCalledWith("Session resumed", "success");
  });

  test("shows toast on agentic_loop_ended", () => {
    const mockUpdateLoop = vi.fn();
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: mockUpdateLoop,
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "agentic_loop_ended",
      loop: { id: "l1", status: "completed", end_reason: "completed" },
    });
    expect(showToast).toHaveBeenCalledWith("Loop ended: completed", "info");
  });

  test("shows error toast on agentic_loop_ended with error reason", () => {
    const mockUpdateLoop = vi.fn();
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: mockUpdateLoop,
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "agentic_loop_ended",
      loop: { id: "l1", status: "error", end_reason: "error" },
    });
    expect(showToast).toHaveBeenCalledWith("Loop ended: error", "error");
  });

  test("shows toast on claude_task_started", () => {
    (useClaudeTaskStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      handleTaskStarted: vi.fn(),
      handleTaskUpdated: vi.fn(),
      handleTaskEnded: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "claude_task_started",
      task_id: "t1",
      session_id: "s1",
      host_id: "h1",
      project_path: "/app",
    });
    expect(showToast).toHaveBeenCalledWith("Claude task started", "info");
  });

  test("shows toast on claude_task_ended completed", () => {
    (useClaudeTaskStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      handleTaskStarted: vi.fn(),
      handleTaskUpdated: vi.fn(),
      handleTaskEnded: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "claude_task_ended",
      task_id: "t1",
      status: "completed",
    });
    expect(showToast).toHaveBeenCalledWith("Claude task completed", "success");
  });

  test("shows error toast on claude_task_ended with error status", () => {
    (useClaudeTaskStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      handleTaskStarted: vi.fn(),
      handleTaskUpdated: vi.fn(),
      handleTaskEnded: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "claude_task_ended",
      task_id: "t1",
      status: "error",
    });
    expect(showToast).toHaveBeenCalledWith("Claude task ended: error", "error");
  });

  test("adds notification for waiting_for_input loop state", () => {
    const mockAddOrUpdate = vi.fn();
    (useNotificationStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      addOrUpdate: mockAddOrUpdate,
      handleLoopResolved: vi.fn(),
      handleToolResolved: vi.fn(),
      notifications: new Map(),
      browserEnabled: false,
    });
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: vi.fn(),
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "agentic_loop_state_update",
      host_id: "h1",
      hostname: "dev-server",
      loop: {
        id: "l1",
        session_id: "s1",
        status: "waiting_for_input",
        tool_name: "claude-code",
        pending_tool_calls: 0,
      },
    });
    expect(mockAddOrUpdate).toHaveBeenCalledWith(
      expect.objectContaining({
        loopId: "l1",
        status: "waiting_for_input",
        hostId: "h1",
        hostname: "dev-server",
        argumentsPreview: null,
      }),
    );
  });

  test("sends browser notification for waiting_for_input when enabled", () => {
    (useNotificationStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      addOrUpdate: vi.fn(),
      handleLoopResolved: vi.fn(),
      handleToolResolved: vi.fn(),
      notifications: new Map(),
      browserEnabled: true,
    });
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: vi.fn(),
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "agentic_loop_state_update",
      loop: {
        id: "l1",
        session_id: "s1",
        status: "waiting_for_input",
        tool_name: "claude-code",
        pending_tool_calls: 0,
      },
    });
    expect(showBrowserNotification).toHaveBeenCalledWith("Claude needs input", {
      body: "claude-code is waiting for your response",
      tag: "loop-l1",
    });
  });

  test("adds notification for pending tool call", () => {
    const mockAddOrUpdate = vi.fn();
    (useNotificationStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      addOrUpdate: mockAddOrUpdate,
      handleLoopResolved: vi.fn(),
      handleToolResolved: vi.fn(),
      notifications: new Map(),
      browserEnabled: false,
    });
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: vi.fn(),
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "agentic_loop_tool_call",
      loop_id: "l1",
      host_id: "h1",
      hostname: "dev",
      tool_call: { id: "tc1", tool_name: "Edit", status: "pending" },
    });
    expect(mockAddOrUpdate).toHaveBeenCalledWith(
      expect.objectContaining({
        loopId: "l1",
        status: "tool_pending",
        latestToolName: "Edit",
        argumentsPreview: null,
      }),
    );
  });

  test("extracts argumentsPreview from tool call arguments_json", () => {
    const mockAddOrUpdate = vi.fn();
    (useNotificationStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      addOrUpdate: mockAddOrUpdate,
      handleLoopResolved: vi.fn(),
      handleToolResolved: vi.fn(),
      notifications: new Map(),
      browserEnabled: false,
    });
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: vi.fn(),
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "agentic_loop_tool_call",
      loop_id: "l1",
      host_id: "h1",
      hostname: "dev",
      tool_call: {
        id: "tc1",
        tool_name: "Bash",
        status: "pending",
        arguments_json: '{"command":"ls -la /tmp"}',
      },
    });
    expect(mockAddOrUpdate).toHaveBeenCalledWith(
      expect.objectContaining({
        argumentsPreview: "ls -la /tmp",
      }),
    );
  });

  test("sends browser notification with preview for pending tool", () => {
    const mockAddOrUpdate = vi.fn();
    (useNotificationStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      addOrUpdate: mockAddOrUpdate,
      handleLoopResolved: vi.fn(),
      handleToolResolved: vi.fn(),
      notifications: new Map(),
      browserEnabled: true,
    });
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: vi.fn(),
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "agentic_loop_tool_call",
      loop_id: "l1",
      tool_call: {
        id: "tc1",
        tool_name: "Read",
        status: "pending",
        arguments_json: '{"file_path":"/src/main.rs"}',
      },
    });
    expect(showBrowserNotification).toHaveBeenCalledWith("Tool call pending", {
      body: "Read: /src/main.rs",
      tag: "loop-l1",
    });
  });

  test("resolves notification when loop ends", () => {
    const mockHandleLoopResolved = vi.fn();
    (useNotificationStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      addOrUpdate: vi.fn(),
      handleLoopResolved: mockHandleLoopResolved,
      handleToolResolved: vi.fn(),
      notifications: new Map(),
      browserEnabled: false,
    });
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: vi.fn(),
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "agentic_loop_ended",
      loop: { id: "l1", status: "completed", end_reason: "done" },
    });
    expect(mockHandleLoopResolved).toHaveBeenCalledWith("l1");
  });

  test("resolves tool notification on tool_result", () => {
    const mockHandleToolResolved = vi.fn();
    (useNotificationStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      addOrUpdate: vi.fn(),
      handleLoopResolved: vi.fn(),
      handleToolResolved: mockHandleToolResolved,
      notifications: new Map(),
      browserEnabled: false,
    });
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: vi.fn(),
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "agentic_loop_tool_result",
      loop_id: "l1",
      tool_call: { id: "tc1", tool_name: "Edit", status: "completed" },
    });
    expect(mockHandleToolResolved).toHaveBeenCalledWith("l1");
  });

  test("resolves notification for working loop state", () => {
    const mockHandleLoopResolved = vi.fn();
    (useNotificationStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      addOrUpdate: vi.fn(),
      handleLoopResolved: mockHandleLoopResolved,
      handleToolResolved: vi.fn(),
      notifications: new Map(),
      browserEnabled: false,
    });
    (useAgenticStore.getState as ReturnType<typeof vi.fn>).mockReturnValue({
      updateLoop: vi.fn(),
      addToolCall: vi.fn(),
      updateToolCall: vi.fn(),
      addTranscript: vi.fn(),
    });

    renderHook(() => useRealtimeUpdates({}));
    mockWs.simulateMessage({
      type: "agentic_loop_state_update",
      loop: { id: "l1", status: "working" },
    });
    expect(mockHandleLoopResolved).toHaveBeenCalledWith("l1");
  });
});
