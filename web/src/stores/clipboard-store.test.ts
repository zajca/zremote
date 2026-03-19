import { describe, test, expect, beforeEach, vi } from "vitest";
import { act } from "@testing-library/react";
import { useClipboardStore } from "./clipboard-store";

let uuidCounter = 0;
vi.stubGlobal("crypto", {
  randomUUID: () => `uuid-${++uuidCounter}`,
});

beforeEach(() => {
  uuidCounter = 0;
  localStorage.clear();
  act(() => {
    useClipboardStore.setState({ entries: [] });
  });
});

describe("addEntry", () => {
  test("creates entry with correct fields", () => {
    act(() =>
      useClipboardStore.getState().addEntry("hello world", {
        sessionId: "s1",
        sessionName: "my-session",
      }),
    );
    const entries = useClipboardStore.getState().entries;
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({
      id: "uuid-1",
      text: "hello world",
      preview: "hello world",
      source: { sessionId: "s1", sessionName: "my-session" },
    });
    expect(typeof entries[0].timestamp).toBe("number");
  });

  test("truncates text longer than 5000 chars", () => {
    const longText = "a".repeat(6000);
    act(() =>
      useClipboardStore.getState().addEntry(longText, { sessionId: "s1" }),
    );
    const entry = useClipboardStore.getState().entries[0];
    expect(entry.text).toHaveLength(5000 + "... (truncated)".length);
    expect(entry.text.endsWith("... (truncated)")).toBe(true);
  });

  test("generates preview with max 100 chars and newlines replaced", () => {
    const text = "line1\nline2\nline3 " + "x".repeat(200);
    act(() =>
      useClipboardStore.getState().addEntry(text, { sessionId: "s1" }),
    );
    const entry = useClipboardStore.getState().entries[0];
    expect(entry.preview).toHaveLength(100);
    expect(entry.preview).not.toContain("\n");
    expect(entry.preview).toContain("line1 line2 line3");
  });

  test("deduplicates same text as last entry by updating timestamp", () => {
    vi.useFakeTimers();
    try {
      vi.setSystemTime(1000);
      act(() =>
        useClipboardStore.getState().addEntry("same text", { sessionId: "s1" }),
      );
      const firstTimestamp = useClipboardStore.getState().entries[0].timestamp;

      vi.setSystemTime(2000);
      act(() =>
        useClipboardStore.getState().addEntry("same text", { sessionId: "s1" }),
      );

      const entries = useClipboardStore.getState().entries;
      expect(entries).toHaveLength(1);
      expect(entries[0].timestamp).toBeGreaterThan(firstTimestamp);
    } finally {
      vi.useRealTimers();
    }
  });

  test("does NOT deduplicate when text differs from last entry", () => {
    act(() =>
      useClipboardStore.getState().addEntry("first", { sessionId: "s1" }),
    );
    act(() =>
      useClipboardStore.getState().addEntry("second", { sessionId: "s1" }),
    );
    expect(useClipboardStore.getState().entries).toHaveLength(2);
  });

  test("evicts oldest entry when at 30 entries", () => {
    act(() => {
      for (let i = 0; i < 35; i++) {
        useClipboardStore.getState().addEntry(`text-${i}`, { sessionId: "s1" });
      }
    });
    const entries = useClipboardStore.getState().entries;
    expect(entries).toHaveLength(30);
    expect(entries[0].text).toBe("text-34");
    expect(entries[29].text).toBe("text-5");
  });

  test("ignores empty or whitespace-only text", () => {
    act(() =>
      useClipboardStore.getState().addEntry("   ", { sessionId: "s1" }),
    );
    expect(useClipboardStore.getState().entries).toHaveLength(0);
  });

  test("persists to localStorage", () => {
    act(() =>
      useClipboardStore.getState().addEntry("persisted", { sessionId: "s1" }),
    );
    const stored = JSON.parse(
      localStorage.getItem("zremote:clipboard-history") ?? "[]",
    );
    expect(stored).toHaveLength(1);
    expect(stored[0].text).toBe("persisted");
  });
});

describe("removeEntry", () => {
  test("removes entry by id", () => {
    act(() => {
      useClipboardStore.getState().addEntry("a", { sessionId: "s1" });
      useClipboardStore.getState().addEntry("b", { sessionId: "s1" });
    });
    const id = useClipboardStore.getState().entries[0].id;
    act(() => useClipboardStore.getState().removeEntry(id));
    expect(useClipboardStore.getState().entries).toHaveLength(1);
    expect(useClipboardStore.getState().entries[0].text).toBe("a");
  });

  test("is no-op for non-existent id", () => {
    act(() =>
      useClipboardStore.getState().addEntry("a", { sessionId: "s1" }),
    );
    act(() => useClipboardStore.getState().removeEntry("nonexistent"));
    expect(useClipboardStore.getState().entries).toHaveLength(1);
  });

  test("updates localStorage", () => {
    act(() =>
      useClipboardStore.getState().addEntry("a", { sessionId: "s1" }),
    );
    const id = useClipboardStore.getState().entries[0].id;
    act(() => useClipboardStore.getState().removeEntry(id));
    const stored = JSON.parse(
      localStorage.getItem("zremote:clipboard-history") ?? "[]",
    );
    expect(stored).toHaveLength(0);
  });
});

describe("clearAll", () => {
  test("empties entries array", () => {
    act(() => {
      useClipboardStore.getState().addEntry("a", { sessionId: "s1" });
      useClipboardStore.getState().addEntry("b", { sessionId: "s1" });
    });
    expect(useClipboardStore.getState().entries).toHaveLength(2);

    act(() => useClipboardStore.getState().clearAll());
    expect(useClipboardStore.getState().entries).toHaveLength(0);
  });

  test("clears localStorage", () => {
    act(() =>
      useClipboardStore.getState().addEntry("a", { sessionId: "s1" }),
    );
    act(() => useClipboardStore.getState().clearAll());
    const stored = JSON.parse(
      localStorage.getItem("zremote:clipboard-history") ?? "[]",
    );
    expect(stored).toHaveLength(0);
  });
});
