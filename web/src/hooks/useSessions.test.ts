import { describe, test, expect, beforeEach, vi } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import { useSessions, SESSION_UPDATE_EVENT } from "./useSessions";

function mockFetchOk(data: unknown) {
  global.fetch = vi.fn().mockResolvedValue({
    ok: true,
    text: async () => JSON.stringify(data),
  });
}

function mockFetchFail() {
  global.fetch = vi.fn().mockResolvedValue({
    ok: false,
    status: 500,
    statusText: "Error",
    text: async () => "server error",
  });
}

beforeEach(() => {
  vi.restoreAllMocks();
});

describe("useSessions", () => {
  test("fetches sessions for host", async () => {
    const sessions = [{ id: "s1", status: "active" }];
    mockFetchOk(sessions);

    const { result } = renderHook(() => useSessions("h1"));
    expect(result.current.loading).toBe(true);

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    expect(result.current.sessions).toEqual(sessions);
    expect(result.current.error).toBeNull();
  });

  test("returns empty sessions when hostId is undefined", async () => {
    const { result } = renderHook(() => useSessions(undefined));

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    expect(result.current.sessions).toEqual([]);
  });

  test("sets error on fetch failure", async () => {
    mockFetchFail();

    const { result } = renderHook(() => useSessions("h1"));

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    expect(result.current.error).toBeInstanceOf(Error);
    expect(result.current.sessions).toEqual([]);
  });

  test("refetches on session update event", async () => {
    const sessions1 = [{ id: "s1" }];
    const sessions2 = [{ id: "s1" }, { id: "s2" }];
    global.fetch = vi.fn()
      .mockResolvedValueOnce({ ok: true, text: async () => JSON.stringify(sessions1) })
      .mockResolvedValueOnce({ ok: true, text: async () => JSON.stringify(sessions2) });

    const { result } = renderHook(() => useSessions("h1"));

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(result.current.sessions).toEqual(sessions1);

    // Dispatch session update event
    await act(async () => {
      window.dispatchEvent(new Event(SESSION_UPDATE_EVENT));
    });

    await waitFor(() => {
      expect(result.current.sessions).toEqual(sessions2);
    });
  });

  test("does not fetch when hostId is undefined", async () => {
    const fetchSpy = vi.fn();
    global.fetch = fetchSpy;

    const { result } = renderHook(() => useSessions(undefined));

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    expect(fetchSpy).not.toHaveBeenCalled();
    expect(result.current.sessions).toEqual([]);
  });

  test("refetch function works", async () => {
    mockFetchOk([{ id: "s1" }]);

    const { result } = renderHook(() => useSessions("h1"));

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    // Mock new data for refetch
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      text: async () => JSON.stringify([{ id: "s1" }, { id: "s2" }]),
    });

    await act(async () => {
      await result.current.refetch();
    });

    expect(result.current.sessions).toHaveLength(2);
  });

  test("wraps non-Error objects in Error", async () => {
    global.fetch = vi.fn().mockRejectedValue("string error");

    const { result } = renderHook(() => useSessions("h1"));

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    expect(result.current.error).toBeInstanceOf(Error);
    expect(result.current.error?.message).toBe("string error");
  });
});
