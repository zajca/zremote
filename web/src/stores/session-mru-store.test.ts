import { describe, expect, test, beforeEach } from "vitest";
import { useSessionMruStore } from "./session-mru-store";
import { act } from "@testing-library/react";

describe("session-mru-store", () => {
  beforeEach(() => {
    localStorage.clear();
    act(() => {
      useSessionMruStore.setState({ mruList: [] });
    });
  });

  test("recordVisit adds session to front", () => {
    act(() => useSessionMruStore.getState().recordVisit("s1"));
    expect(useSessionMruStore.getState().mruList).toEqual(["s1"]);

    act(() => useSessionMruStore.getState().recordVisit("s2"));
    expect(useSessionMruStore.getState().mruList).toEqual(["s2", "s1"]);
  });

  test("recordVisit moves existing session to front", () => {
    act(() => {
      useSessionMruStore.getState().recordVisit("s1");
      useSessionMruStore.getState().recordVisit("s2");
      useSessionMruStore.getState().recordVisit("s3");
    });
    expect(useSessionMruStore.getState().mruList).toEqual(["s3", "s2", "s1"]);

    act(() => useSessionMruStore.getState().recordVisit("s1"));
    expect(useSessionMruStore.getState().mruList).toEqual(["s1", "s3", "s2"]);
  });

  test("recordVisit deduplicates", () => {
    act(() => {
      useSessionMruStore.getState().recordVisit("s1");
      useSessionMruStore.getState().recordVisit("s1");
    });
    expect(useSessionMruStore.getState().mruList).toEqual(["s1"]);
  });

  test("removeSession removes from list", () => {
    act(() => {
      useSessionMruStore.getState().recordVisit("s1");
      useSessionMruStore.getState().recordVisit("s2");
      useSessionMruStore.getState().recordVisit("s3");
    });
    expect(useSessionMruStore.getState().mruList).toEqual(["s3", "s2", "s1"]);

    act(() => useSessionMruStore.getState().removeSession("s2"));
    expect(useSessionMruStore.getState().mruList).toEqual(["s3", "s1"]);
  });

  test("removeSession ignores non-existent session", () => {
    act(() => {
      useSessionMruStore.getState().recordVisit("s1");
    });
    act(() => useSessionMruStore.getState().removeSession("s99"));
    expect(useSessionMruStore.getState().mruList).toEqual(["s1"]);
  });

  test("caps at 50 entries", () => {
    act(() => {
      for (let i = 0; i < 60; i++) {
        useSessionMruStore.getState().recordVisit(`s${i}`);
      }
    });
    const list = useSessionMruStore.getState().mruList;
    expect(list).toHaveLength(50);
    expect(list[0]).toBe("s59");
    expect(list[49]).toBe("s10");
  });

  test("persists to localStorage", () => {
    act(() => {
      useSessionMruStore.getState().recordVisit("s1");
      useSessionMruStore.getState().recordVisit("s2");
    });
    const stored = JSON.parse(
      localStorage.getItem("zremote:session-mru") ?? "[]",
    );
    expect(stored).toEqual(["s2", "s1"]);
  });

  test("removeSession updates localStorage", () => {
    act(() => {
      useSessionMruStore.getState().recordVisit("s1");
      useSessionMruStore.getState().recordVisit("s2");
    });
    act(() => useSessionMruStore.getState().removeSession("s1"));
    const stored = JSON.parse(
      localStorage.getItem("zremote:session-mru") ?? "[]",
    );
    expect(stored).toEqual(["s2"]);
  });
});
