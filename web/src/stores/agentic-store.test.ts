import { describe, test, expect, beforeEach, vi } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useAgenticStore } from "./agentic-store";
import type { AgenticLoop, ToolCall, TranscriptEntry } from "../types/agentic";

const mockLoop: AgenticLoop = {
  id: "loop-1",
  session_id: "s1",
  project_path: "/app",
  tool_name: "claude",
  model: "opus",
  status: "working",
  started_at: "2026-01-01T00:00:00Z",
  ended_at: null,
  total_tokens_in: 100,
  total_tokens_out: 50,
  estimated_cost_usd: 0.01,
  end_reason: null,
  summary: null,
  context_used: 1000,
  context_max: 200000,
  pending_tool_calls: 0,
};

const mockToolCall: ToolCall = {
  id: "tc-1",
  loop_id: "loop-1",
  tool_name: "Read",
  arguments_json: '{"path": "/file.ts"}',
  status: "pending",
  result_preview: null,
  duration_ms: null,
  created_at: "2026-01-01T00:00:00Z",
  resolved_at: null,
};

const mockTranscript: TranscriptEntry = {
  id: 1,
  loop_id: "loop-1",
  role: "assistant",
  content: "Hello",
  tool_call_id: null,
  timestamp: "2026-01-01T00:00:00Z",
};

beforeEach(() => {
  vi.restoreAllMocks();
  // Reset store state
  useAgenticStore.setState({
    activeLoops: new Map(),
    toolCalls: new Map(),
    transcripts: new Map(),
  });
});

describe("initial state", () => {
  test("has empty maps", () => {
    const { result } = renderHook(() => useAgenticStore());
    expect(result.current.activeLoops.size).toBe(0);
    expect(result.current.toolCalls.size).toBe(0);
    expect(result.current.transcripts.size).toBe(0);
  });
});

describe("updateLoop", () => {
  test("adds a new loop", () => {
    const { result } = renderHook(() => useAgenticStore());
    act(() => result.current.updateLoop(mockLoop));
    expect(result.current.activeLoops.get("loop-1")).toEqual(mockLoop);
  });

  test("updates existing loop", () => {
    const { result } = renderHook(() => useAgenticStore());
    act(() => result.current.updateLoop(mockLoop));
    const updated = { ...mockLoop, status: "completed" as const };
    act(() => result.current.updateLoop(updated));
    expect(result.current.activeLoops.get("loop-1")?.status).toBe("completed");
    expect(result.current.activeLoops.size).toBe(1);
  });
});

describe("removeLoop", () => {
  test("removes loop and associated data", () => {
    const { result } = renderHook(() => useAgenticStore());
    act(() => {
      result.current.updateLoop(mockLoop);
      result.current.addToolCall("loop-1", mockToolCall);
      result.current.addTranscript("loop-1", mockTranscript);
    });
    expect(result.current.activeLoops.size).toBe(1);
    expect(result.current.toolCalls.size).toBe(1);
    expect(result.current.transcripts.size).toBe(1);

    act(() => result.current.removeLoop("loop-1"));
    expect(result.current.activeLoops.size).toBe(0);
    expect(result.current.toolCalls.size).toBe(0);
    expect(result.current.transcripts.size).toBe(0);
  });

  test("no-op for non-existent loop", () => {
    const { result } = renderHook(() => useAgenticStore());
    act(() => result.current.removeLoop("nonexistent"));
    expect(result.current.activeLoops.size).toBe(0);
  });
});

describe("addToolCall", () => {
  test("adds tool call to new loop", () => {
    const { result } = renderHook(() => useAgenticStore());
    act(() => result.current.addToolCall("loop-1", mockToolCall));
    expect(result.current.toolCalls.get("loop-1")).toEqual([mockToolCall]);
  });

  test("appends tool call to existing loop", () => {
    const { result } = renderHook(() => useAgenticStore());
    const tc2 = { ...mockToolCall, id: "tc-2" };
    act(() => {
      result.current.addToolCall("loop-1", mockToolCall);
      result.current.addToolCall("loop-1", tc2);
    });
    expect(result.current.toolCalls.get("loop-1")).toHaveLength(2);
  });
});

describe("updateToolCall", () => {
  test("updates existing tool call by id", () => {
    const { result } = renderHook(() => useAgenticStore());
    act(() => result.current.addToolCall("loop-1", mockToolCall));

    const updated = { ...mockToolCall, status: "completed" as const, duration_ms: 100 };
    act(() => result.current.updateToolCall("loop-1", updated));

    const calls = result.current.toolCalls.get("loop-1")!;
    expect(calls).toHaveLength(1);
    expect(calls[0].status).toBe("completed");
    expect(calls[0].duration_ms).toBe(100);
  });

  test("appends when tool call id not found", () => {
    const { result } = renderHook(() => useAgenticStore());
    act(() => result.current.addToolCall("loop-1", mockToolCall));

    const newTc = { ...mockToolCall, id: "tc-new", status: "running" as const };
    act(() => result.current.updateToolCall("loop-1", newTc));

    expect(result.current.toolCalls.get("loop-1")).toHaveLength(2);
  });

  test("adds to empty loop", () => {
    const { result } = renderHook(() => useAgenticStore());
    act(() => result.current.updateToolCall("loop-1", mockToolCall));
    expect(result.current.toolCalls.get("loop-1")).toEqual([mockToolCall]);
  });
});

describe("addTranscript", () => {
  test("adds transcript entry", () => {
    const { result } = renderHook(() => useAgenticStore());
    act(() => result.current.addTranscript("loop-1", mockTranscript));
    expect(result.current.transcripts.get("loop-1")).toEqual([mockTranscript]);
  });

  test("appends to existing entries", () => {
    const { result } = renderHook(() => useAgenticStore());
    const entry2 = { ...mockTranscript, id: 2, role: "user" as const, content: "World" };
    act(() => {
      result.current.addTranscript("loop-1", mockTranscript);
      result.current.addTranscript("loop-1", entry2);
    });
    expect(result.current.transcripts.get("loop-1")).toHaveLength(2);
  });
});

describe("fetchLoop", () => {
  test("fetches loop from API and updates store", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => JSON.stringify(mockLoop),
    });

    const { result } = renderHook(() => useAgenticStore());
    await act(async () => {
      await result.current.fetchLoop("loop-1");
    });
    expect(result.current.activeLoops.get("loop-1")).toEqual(mockLoop);
    expect(fetch).toHaveBeenCalledWith("/api/loops/loop-1", expect.any(Object));
  });
});

describe("fetchToolCalls", () => {
  test("fetches tool calls and replaces in store", async () => {
    const calls = [mockToolCall, { ...mockToolCall, id: "tc-2" }];
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => JSON.stringify(calls),
    });

    const { result } = renderHook(() => useAgenticStore());
    await act(async () => {
      await result.current.fetchToolCalls("loop-1");
    });
    expect(result.current.toolCalls.get("loop-1")).toHaveLength(2);
  });
});

describe("fetchTranscript", () => {
  test("fetches transcript and replaces in store", async () => {
    const entries = [mockTranscript];
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => JSON.stringify(entries),
    });

    const { result } = renderHook(() => useAgenticStore());
    await act(async () => {
      await result.current.fetchTranscript("loop-1");
    });
    expect(result.current.transcripts.get("loop-1")).toEqual(entries);
  });
});

describe("sendAction", () => {
  test("sends action to API", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => "",
    });

    const { result } = renderHook(() => useAgenticStore());
    await act(async () => {
      await result.current.sendAction("loop-1", "approve", "yes");
    });
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(call[0]).toBe("/api/loops/loop-1/action");
    expect(JSON.parse(call[1].body)).toEqual({ action: "approve", payload: "yes" });
  });

  test("throws on API error", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: false,
      status: 500,
      statusText: "Error",
      text: async () => "server error",
    });

    const { result } = renderHook(() => useAgenticStore());
    await expect(
      act(async () => {
        await result.current.sendAction("loop-1", "stop");
      }),
    ).rejects.toThrow();
  });
});
