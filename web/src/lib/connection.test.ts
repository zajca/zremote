import { describe, test, expect, beforeEach, vi } from "vitest";

// We need to re-import with fresh module for each test since detectMode caches
beforeEach(() => {
  vi.restoreAllMocks();
  vi.resetModules();
});

describe("detectMode", () => {
  test("returns 'local' when server reports local mode", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      json: async () => ({ mode: "local" }),
    });
    const { detectMode } = await import("./connection");
    const mode = await detectMode();
    expect(mode).toBe("local");
    expect(fetch).toHaveBeenCalledWith("/api/mode");
  });

  test("returns 'server' when server reports server mode", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      json: async () => ({ mode: "server" }),
    });
    const { detectMode } = await import("./connection");
    const mode = await detectMode();
    expect(mode).toBe("server");
  });

  test("returns cached value on second call", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      json: async () => ({ mode: "local" }),
    });
    const { detectMode } = await import("./connection");
    await detectMode();
    const mode2 = await detectMode();
    expect(mode2).toBe("local");
    // fetch should only be called once
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  test("returns 'server' as fallback for unknown mode", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      json: async () => ({ mode: "unknown" }),
    });
    const { detectMode } = await import("./connection");
    const mode = await detectMode();
    expect(mode).toBe("server");
  });

  test("returns 'server' on fetch error", async () => {
    global.fetch = vi.fn().mockRejectedValueOnce(new Error("network error"));
    const { detectMode } = await import("./connection");
    const mode = await detectMode();
    expect(mode).toBe("server");
  });
});
