import { describe, test, expect, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { usePendingPasteStore } from "./pending-paste-store";

beforeEach(() => {
  usePendingPasteStore.setState({ pendingPaste: null });
});

describe("usePendingPasteStore", () => {
  test("initial state is null", () => {
    const { result } = renderHook(() => usePendingPasteStore());
    expect(result.current.pendingPaste).toBeNull();
  });

  test("setPendingPaste stores sessionId and data", () => {
    const { result } = renderHook(() => usePendingPasteStore());
    act(() => result.current.setPendingPaste("sess-1", "echo hello"));
    expect(result.current.pendingPaste).toEqual({
      sessionId: "sess-1",
      data: "echo hello",
    });
  });

  test("consume returns data for matching sessionId and clears state", () => {
    const { result } = renderHook(() => usePendingPasteStore());
    act(() => result.current.setPendingPaste("sess-1", "echo hello"));

    let consumed: string | null = null;
    act(() => {
      consumed = result.current.consume("sess-1");
    });

    expect(consumed).toBe("echo hello");
    expect(result.current.pendingPaste).toBeNull();
  });

  test("consume returns null for non-matching sessionId", () => {
    const { result } = renderHook(() => usePendingPasteStore());
    act(() => result.current.setPendingPaste("sess-1", "echo hello"));

    let consumed: string | null = null;
    act(() => {
      consumed = result.current.consume("sess-other");
    });

    expect(consumed).toBeNull();
    // Original paste should still be there
    expect(result.current.pendingPaste).toEqual({
      sessionId: "sess-1",
      data: "echo hello",
    });
  });

  test("consume returns null when no pending paste", () => {
    const { result } = renderHook(() => usePendingPasteStore());

    let consumed: string | null = null;
    act(() => {
      consumed = result.current.consume("sess-1");
    });

    expect(consumed).toBeNull();
  });

  test("setPendingPaste overwrites previous paste", () => {
    const { result } = renderHook(() => usePendingPasteStore());
    act(() => result.current.setPendingPaste("sess-1", "first"));
    act(() => result.current.setPendingPaste("sess-2", "second"));

    expect(result.current.pendingPaste).toEqual({
      sessionId: "sess-2",
      data: "second",
    });
  });

  test("consume is idempotent - second call returns null", () => {
    const { result } = renderHook(() => usePendingPasteStore());
    act(() => result.current.setPendingPaste("sess-1", "data"));

    let first: string | null = null;
    let second: string | null = null;
    act(() => {
      first = result.current.consume("sess-1");
    });
    act(() => {
      second = result.current.consume("sess-1");
    });

    expect(first).toBe("data");
    expect(second).toBeNull();
  });
});
