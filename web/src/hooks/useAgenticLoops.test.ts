import { describe, test, expect, beforeEach, vi, afterEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import { useAgenticLoops, AGENTIC_LOOP_UPDATE_EVENT } from "./useAgenticLoops";

beforeEach(() => {
  vi.restoreAllMocks();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useAgenticLoops", () => {
  test("fetches loops for session", async () => {
    const loops = [{ id: "l1", status: "working" }];
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      text: async () => JSON.stringify(loops),
    });

    const { result } = renderHook(() => useAgenticLoops("s1"));

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    expect(result.current.loops).toEqual(loops);
    expect(fetch).toHaveBeenCalledWith("/api/loops?session_id=s1", expect.any(Object));
  });

  test("returns empty loops when sessionId is undefined", () => {
    const { result } = renderHook(() => useAgenticLoops(undefined));
    expect(result.current.loops).toEqual([]);
    expect(result.current.loading).toBe(false);
  });

  test("handles fetch error gracefully", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 500,
      statusText: "Error",
      text: async () => "error",
    });
    vi.spyOn(console, "warn").mockImplementation(() => {});

    const { result } = renderHook(() => useAgenticLoops("s1"));

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    expect(result.current.loops).toEqual([]);
  });

  test("refetches on agentic loop update event", async () => {
    const loops1 = [{ id: "l1" }];
    const loops2 = [{ id: "l1" }, { id: "l2" }];
    global.fetch = vi.fn()
      .mockResolvedValueOnce({ ok: true, text: async () => JSON.stringify(loops1) })
      .mockResolvedValueOnce({ ok: true, text: async () => JSON.stringify(loops2) });

    const { result } = renderHook(() => useAgenticLoops("s1"));

    await waitFor(() => {
      expect(result.current.loops).toEqual(loops1);
    });

    await act(async () => {
      window.dispatchEvent(new Event(AGENTIC_LOOP_UPDATE_EVENT));
    });

    await waitFor(() => {
      expect(result.current.loops).toEqual(loops2);
    });
  });

  test("polls every 15 seconds", async () => {
    vi.useFakeTimers();
    const loops = [{ id: "l1" }];
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      text: async () => JSON.stringify(loops),
    });

    renderHook(() => useAgenticLoops("s1"));

    // Let initial fetch resolve
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const callsAfterInit = (fetch as ReturnType<typeof vi.fn>).mock.calls.length;

    // Advance 15 seconds for polling
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15000);
    });

    expect((fetch as ReturnType<typeof vi.fn>).mock.calls.length).toBeGreaterThan(callsAfterInit);
  });

  test("refetch function works", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      text: async () => JSON.stringify([{ id: "l1" }]),
    });

    const { result } = renderHook(() => useAgenticLoops("s1"));

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => JSON.stringify([{ id: "l1" }, { id: "l2" }]),
    });

    await act(async () => {
      await result.current.refetch();
    });

    expect(result.current.loops).toHaveLength(2);
  });

  test("clears loops when sessionId changes to undefined", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      text: async () => JSON.stringify([{ id: "l1" }]),
    });

    const { result, rerender } = renderHook(
      ({ sessionId }) => useAgenticLoops(sessionId),
      { initialProps: { sessionId: "s1" as string | undefined } },
    );

    await waitFor(() => {
      expect(result.current.loops).toHaveLength(1);
    });

    rerender({ sessionId: undefined });
    expect(result.current.loops).toEqual([]);
  });
});
