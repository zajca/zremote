import { describe, expect, test, beforeEach } from "vitest";
import { useCommandPaletteStore } from "./command-palette-store";
import { act } from "@testing-library/react";

describe("command-palette-store", () => {
  beforeEach(() => {
    // Reset store to defaults
    act(() => {
      useCommandPaletteStore.setState({
        open: false,
        contextStack: [{ level: "global" }],
        query: "",
      });
    });
  });

  test("open/close", () => {
    const store = useCommandPaletteStore.getState();
    expect(store.open).toBe(false);

    act(() => store.setOpen(true));
    expect(useCommandPaletteStore.getState().open).toBe(true);

    act(() => store.setOpen(false));
    expect(useCommandPaletteStore.getState().open).toBe(false);
  });

  test("toggle", () => {
    const store = useCommandPaletteStore.getState();
    expect(store.open).toBe(false);

    act(() => store.toggle());
    expect(useCommandPaletteStore.getState().open).toBe(true);

    act(() => useCommandPaletteStore.getState().toggle());
    expect(useCommandPaletteStore.getState().open).toBe(false);
  });

  test("push context onto stack", () => {
    const store = useCommandPaletteStore.getState();

    act(() => store.pushContext({ level: "host", hostId: "h1", hostName: "myhost" }));
    const state = useCommandPaletteStore.getState();
    expect(state.contextStack).toHaveLength(2);
    expect(state.contextStack[0]).toEqual({ level: "global" });
    expect(state.contextStack[1]).toEqual({ level: "host", hostId: "h1", hostName: "myhost" });
    expect(state.query).toBe("");
  });

  test("push context clears query", () => {
    const store = useCommandPaletteStore.getState();
    act(() => store.setQuery("test"));
    expect(useCommandPaletteStore.getState().query).toBe("test");

    act(() => useCommandPaletteStore.getState().pushContext({ level: "host", hostId: "h1" }));
    expect(useCommandPaletteStore.getState().query).toBe("");
  });

  test("pop context removes last item", () => {
    const store = useCommandPaletteStore.getState();

    act(() => {
      store.pushContext({ level: "host", hostId: "h1" });
      useCommandPaletteStore.getState().pushContext({ level: "session", hostId: "h1", sessionId: "s1" });
    });

    expect(useCommandPaletteStore.getState().contextStack).toHaveLength(3);

    act(() => useCommandPaletteStore.getState().popContext());
    const state = useCommandPaletteStore.getState();
    expect(state.contextStack).toHaveLength(2);
    expect(state.contextStack[1].level).toBe("host");
  });

  test("popContext does nothing when stack has 1 item", () => {
    const store = useCommandPaletteStore.getState();
    expect(store.contextStack).toHaveLength(1);

    act(() => store.popContext());
    expect(useCommandPaletteStore.getState().contextStack).toHaveLength(1);
    expect(useCommandPaletteStore.getState().contextStack[0]).toEqual({ level: "global" });
  });

  test("popContext clears query", () => {
    const store = useCommandPaletteStore.getState();
    act(() => {
      store.pushContext({ level: "host", hostId: "h1" });
      useCommandPaletteStore.getState().setQuery("test");
    });
    expect(useCommandPaletteStore.getState().query).toBe("test");

    act(() => useCommandPaletteStore.getState().popContext());
    expect(useCommandPaletteStore.getState().query).toBe("");
  });

  test("jumpToIndex slices stack", () => {
    const store = useCommandPaletteStore.getState();
    act(() => {
      store.pushContext({ level: "host", hostId: "h1" });
      useCommandPaletteStore.getState().pushContext({ level: "session", hostId: "h1", sessionId: "s1" });
      useCommandPaletteStore.getState().pushContext({ level: "loop", hostId: "h1", sessionId: "s1", loopId: "l1" });
    });

    expect(useCommandPaletteStore.getState().contextStack).toHaveLength(4);

    act(() => useCommandPaletteStore.getState().jumpToIndex(1));
    const state = useCommandPaletteStore.getState();
    expect(state.contextStack).toHaveLength(2);
    expect(state.contextStack[1].level).toBe("host");
    expect(state.query).toBe("");
  });

  test("jumpToIndex to 0 resets to first item", () => {
    const store = useCommandPaletteStore.getState();
    act(() => {
      store.pushContext({ level: "host", hostId: "h1" });
    });

    act(() => useCommandPaletteStore.getState().jumpToIndex(0));
    expect(useCommandPaletteStore.getState().contextStack).toHaveLength(1);
    expect(useCommandPaletteStore.getState().contextStack[0]).toEqual({ level: "global" });
  });

  test("jumpToIndex ignores invalid index", () => {
    const store = useCommandPaletteStore.getState();
    act(() => {
      store.pushContext({ level: "host", hostId: "h1" });
    });

    act(() => useCommandPaletteStore.getState().jumpToIndex(5));
    expect(useCommandPaletteStore.getState().contextStack).toHaveLength(2);

    act(() => useCommandPaletteStore.getState().jumpToIndex(-1));
    expect(useCommandPaletteStore.getState().contextStack).toHaveLength(2);
  });

  test("resetToRouteContext replaces stack", () => {
    const store = useCommandPaletteStore.getState();
    act(() => {
      store.pushContext({ level: "host", hostId: "h1" });
      useCommandPaletteStore.getState().pushContext({ level: "session", hostId: "h1", sessionId: "s1" });
      useCommandPaletteStore.getState().setQuery("hello");
    });

    act(() => {
      useCommandPaletteStore.getState().resetToRouteContext({ level: "project", projectId: "p1" });
    });

    const state = useCommandPaletteStore.getState();
    expect(state.contextStack).toHaveLength(1);
    expect(state.contextStack[0]).toEqual({ level: "project", projectId: "p1" });
    expect(state.query).toBe("");
  });

  test("query management", () => {
    const store = useCommandPaletteStore.getState();
    expect(store.query).toBe("");

    act(() => store.setQuery("hello"));
    expect(useCommandPaletteStore.getState().query).toBe("hello");

    act(() => useCommandPaletteStore.getState().setQuery(""));
    expect(useCommandPaletteStore.getState().query).toBe("");
  });

  test("currentContext returns top of stack", () => {
    const store = useCommandPaletteStore.getState();
    expect(store.currentContext()).toEqual({ level: "global" });

    act(() => store.pushContext({ level: "host", hostId: "h1" }));
    expect(useCommandPaletteStore.getState().currentContext()).toEqual({ level: "host", hostId: "h1" });

    act(() => useCommandPaletteStore.getState().pushContext({ level: "session", hostId: "h1", sessionId: "s1" }));
    expect(useCommandPaletteStore.getState().currentContext()).toEqual({
      level: "session",
      hostId: "h1",
      sessionId: "s1",
    });
  });
});
