import { describe, test, expect, beforeEach, vi } from "vitest";
import { act } from "@testing-library/react";
import { useActiveTerminalStore } from "./active-terminal-store";

beforeEach(() => {
  act(() => {
    useActiveTerminalStore.setState({ sessionId: null, sendInput: null });
  });
});

describe("useActiveTerminalStore", () => {
  test("initial state has null sessionId and null sendInput", () => {
    const state = useActiveTerminalStore.getState();
    expect(state.sessionId).toBeNull();
    expect(state.sendInput).toBeNull();
  });

  test("register sets sessionId and sendInput", () => {
    const sender = vi.fn();
    act(() => useActiveTerminalStore.getState().register("s1", sender));

    const state = useActiveTerminalStore.getState();
    expect(state.sessionId).toBe("s1");
    expect(state.sendInput).toBe(sender);
  });

  test("unregister clears state when sessionId matches", () => {
    const sender = vi.fn();
    act(() => useActiveTerminalStore.getState().register("s1", sender));
    act(() => useActiveTerminalStore.getState().unregister("s1"));

    const state = useActiveTerminalStore.getState();
    expect(state.sessionId).toBeNull();
    expect(state.sendInput).toBeNull();
  });

  test("unregister does NOT clear state when sessionId does not match", () => {
    const sender = vi.fn();
    act(() => useActiveTerminalStore.getState().register("s1", sender));
    act(() => useActiveTerminalStore.getState().unregister("s2"));

    const state = useActiveTerminalStore.getState();
    expect(state.sessionId).toBe("s1");
    expect(state.sendInput).toBe(sender);
  });

  test("register overwrites previous registration", () => {
    const sender1 = vi.fn();
    const sender2 = vi.fn();
    act(() => useActiveTerminalStore.getState().register("s1", sender1));
    act(() => useActiveTerminalStore.getState().register("s2", sender2));

    const state = useActiveTerminalStore.getState();
    expect(state.sessionId).toBe("s2");
    expect(state.sendInput).toBe(sender2);
  });
});
