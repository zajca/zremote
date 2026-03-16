import { describe, test, expect, beforeEach, vi } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useKnowledgeStore } from "./knowledge-store";

function mockFetchOk(data: unknown) {
  const text = data === undefined ? "" : JSON.stringify(data);
  global.fetch = vi.fn().mockResolvedValueOnce({
    ok: true,
    text: async () => text,
  });
}

function mockFetchFail() {
  global.fetch = vi.fn().mockResolvedValueOnce({
    ok: false,
    status: 500,
    statusText: "Error",
    text: async () => "server error",
  });
}

beforeEach(() => {
  vi.restoreAllMocks();
  useKnowledgeStore.setState({
    statusByProject: {},
    memoriesByProject: {},
    searchResults: [],
    searchLoading: false,
    indexingProgress: {},
    bootstrapStatus: {},
  });
});

describe("initial state", () => {
  test("has empty state", () => {
    const state = useKnowledgeStore.getState();
    expect(state.searchResults).toEqual([]);
    expect(state.searchLoading).toBe(false);
  });
});

describe("fetchStatus", () => {
  test("fetches and stores status", async () => {
    const status = { id: "kb1", status: "ready" };
    mockFetchOk(status);

    await useKnowledgeStore.getState().fetchStatus("p1");
    expect(useKnowledgeStore.getState().statusByProject["p1"]).toEqual(status);
  });

  test("handles error gracefully", async () => {
    mockFetchFail();
    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    await useKnowledgeStore.getState().fetchStatus("p1");
    expect(consoleSpy).toHaveBeenCalled();
    expect(useKnowledgeStore.getState().statusByProject["p1"]).toBeUndefined();
  });
});

describe("fetchMemories", () => {
  test("fetches and stores memories", async () => {
    const memories = [{ id: "m1", key: "test", content: "data" }];
    mockFetchOk(memories);

    await useKnowledgeStore.getState().fetchMemories("p1");
    expect(useKnowledgeStore.getState().memoriesByProject["p1"]).toEqual(memories);
  });

  test("passes category filter", async () => {
    mockFetchOk([]);

    await useKnowledgeStore.getState().fetchMemories("p1", "pattern");
    expect(fetch).toHaveBeenCalledWith(
      "/api/projects/p1/knowledge/memories?category=pattern",
      expect.any(Object),
    );
  });

  test("handles error gracefully", async () => {
    mockFetchFail();
    vi.spyOn(console, "error").mockImplementation(() => {});

    await useKnowledgeStore.getState().fetchMemories("p1");
    expect(useKnowledgeStore.getState().memoriesByProject["p1"]).toBeUndefined();
  });
});

describe("search", () => {
  test("searches and stores results", async () => {
    const response = { results: [{ path: "/file.ts", score: 0.9 }], duration_ms: 10 };
    mockFetchOk(response);

    await useKnowledgeStore.getState().search("p1", "query");
    const state = useKnowledgeStore.getState();
    expect(state.searchResults).toEqual(response.results);
    expect(state.searchLoading).toBe(false);
  });

  test("clears results on error", async () => {
    mockFetchFail();
    vi.spyOn(console, "error").mockImplementation(() => {});

    await useKnowledgeStore.getState().search("p1", "query");
    const state = useKnowledgeStore.getState();
    expect(state.searchResults).toEqual([]);
    expect(state.searchLoading).toBe(false);
  });
});

describe("triggerIndex", () => {
  test("calls API", async () => {
    mockFetchOk(undefined);
    await useKnowledgeStore.getState().triggerIndex("p1");
    expect(fetch).toHaveBeenCalled();
  });

  test("throws on error", async () => {
    mockFetchFail();
    vi.spyOn(console, "error").mockImplementation(() => {});
    await expect(useKnowledgeStore.getState().triggerIndex("p1")).rejects.toThrow();
  });
});

describe("extractMemories", () => {
  test("calls API with project and loop id", async () => {
    mockFetchOk(undefined);
    await useKnowledgeStore.getState().extractMemories("p1", "l1");
    const call = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(JSON.parse(call[1].body)).toEqual({ loop_id: "l1" });
  });

  test("throws on error", async () => {
    mockFetchFail();
    vi.spyOn(console, "error").mockImplementation(() => {});
    await expect(useKnowledgeStore.getState().extractMemories("p1", "l1")).rejects.toThrow();
  });
});

describe("deleteMemory", () => {
  test("deletes and refetches memories", async () => {
    global.fetch = vi.fn()
      .mockResolvedValueOnce({ ok: true, text: async () => "" })
      .mockResolvedValueOnce({ ok: true, text: async () => JSON.stringify([]) });

    await useKnowledgeStore.getState().deleteMemory("p1", "m1");
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  test("throws on error", async () => {
    mockFetchFail();
    vi.spyOn(console, "error").mockImplementation(() => {});
    await expect(useKnowledgeStore.getState().deleteMemory("p1", "m1")).rejects.toThrow();
  });
});

describe("updateMemory", () => {
  test("updates memory in store", async () => {
    const updatedMemory = {
      id: "m1",
      project_id: "p1",
      loop_id: null,
      key: "test",
      content: "updated",
      category: "pattern" as const,
      confidence: 0.9,
      created_at: "2026-01-01T00:00:00Z",
      updated_at: "2026-01-02T00:00:00Z",
    };
    mockFetchOk(updatedMemory);

    // Pre-populate
    useKnowledgeStore.setState({
      memoriesByProject: { p1: [{ ...updatedMemory, content: "original" }] },
    });

    await useKnowledgeStore.getState().updateMemory("p1", "m1", { content: "updated" });
    expect(useKnowledgeStore.getState().memoriesByProject["p1"]![0].content).toBe("updated");
  });

  test("throws on error", async () => {
    mockFetchFail();
    vi.spyOn(console, "error").mockImplementation(() => {});
    await expect(useKnowledgeStore.getState().updateMemory("p1", "m1", { content: "x" })).rejects.toThrow();
  });
});

describe("controlService", () => {
  test("calls API", async () => {
    mockFetchOk(undefined);
    await useKnowledgeStore.getState().controlService("h1", "restart");
    expect(fetch).toHaveBeenCalled();
  });

  test("throws on error", async () => {
    mockFetchFail();
    vi.spyOn(console, "error").mockImplementation(() => {});
    await expect(useKnowledgeStore.getState().controlService("h1", "stop")).rejects.toThrow();
  });
});

describe("generateInstructions", () => {
  test("calls API", async () => {
    mockFetchOk(undefined);
    await useKnowledgeStore.getState().generateInstructions("p1");
    expect(fetch).toHaveBeenCalled();
  });
});

describe("bootstrapProject", () => {
  test("sets status to running then done", async () => {
    global.fetch = vi.fn()
      .mockResolvedValueOnce({ ok: true, text: async () => "" })
      .mockResolvedValueOnce({ ok: true, text: async () => JSON.stringify({ id: "kb1" }) });

    await useKnowledgeStore.getState().bootstrapProject("p1");
    expect(useKnowledgeStore.getState().bootstrapStatus["p1"]).toBe("done");
  });

  test("sets status to error on failure", async () => {
    mockFetchFail();
    vi.spyOn(console, "error").mockImplementation(() => {});

    await expect(useKnowledgeStore.getState().bootstrapProject("p1")).rejects.toThrow();
    expect(useKnowledgeStore.getState().bootstrapStatus["p1"]).toBe("error");
  });
});

describe("event handlers", () => {
  test("handleKnowledgeStatusChanged is callable", () => {
    useKnowledgeStore.getState().handleKnowledgeStatusChanged("h1", "ready", null);
    // No-op function, just verifies it doesn't throw
  });

  test("handleIndexingProgress stores progress", () => {
    const progress = {
      project_id: "p1",
      project_path: "/app",
      status: "in_progress" as const,
      files_processed: 5,
      files_total: 10,
    };

    useKnowledgeStore.getState().handleIndexingProgress(progress);
    expect(useKnowledgeStore.getState().indexingProgress["p1"]).toEqual(progress);
  });

  test("handleMemoryExtracted triggers fetchMemories", () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => JSON.stringify([]),
    });

    useKnowledgeStore.getState().handleMemoryExtracted("p1");
    expect(fetch).toHaveBeenCalled();
  });
});
