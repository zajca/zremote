import { describe, test, expect, vi, beforeEach } from "vitest";
import {
  extractArgsPreview,
  extractProjectName,
  resolveSessionName,
  getCachedSessionName,
  clearSessionNameCache,
} from "./notification-utils";

vi.mock("./api", () => ({
  api: {
    sessions: {
      get: vi.fn(),
    },
  },
}));

import { api } from "./api";

describe("extractArgsPreview", () => {
  test("returns null for null input", () => {
    expect(extractArgsPreview(null)).toBeNull();
  });

  test("extracts command from Bash JSON", () => {
    const json = JSON.stringify({ command: "ls -la /tmp" });
    expect(extractArgsPreview(json)).toBe("ls -la /tmp");
  });

  test("extracts file_path from Read JSON", () => {
    const json = JSON.stringify({ file_path: "/src/app.ts" });
    expect(extractArgsPreview(json)).toBe("/src/app.ts");
  });

  test("extracts first string value from Edit JSON", () => {
    const json = JSON.stringify({
      file_path: "/bar.ts",
      old_string: "foo",
      new_string: "bar",
    });
    expect(extractArgsPreview(json)).toBe("/bar.ts");
  });

  test("truncates long strings with ellipsis", () => {
    const longCommand = "a".repeat(100);
    const json = JSON.stringify({ command: longCommand });
    const result = extractArgsPreview(json);
    expect(result).toBe("a".repeat(80) + "...");
  });

  test("respects custom maxLen", () => {
    const json = JSON.stringify({ command: "a".repeat(50) });
    const result = extractArgsPreview(json, 20);
    expect(result).toBe("a".repeat(20) + "...");
  });

  test("falls back to raw JSON for malformed input", () => {
    expect(extractArgsPreview("not-json")).toBe("not-json");
  });

  test("truncates long raw JSON fallback", () => {
    const raw = "x".repeat(100);
    expect(extractArgsPreview(raw)).toBe("x".repeat(80) + "...");
  });

  test("skips non-string values", () => {
    const json = JSON.stringify({ count: 42, verbose: true, path: "/foo" });
    expect(extractArgsPreview(json)).toBe("/foo");
  });

  test("falls back to raw for object with no string values", () => {
    const json = JSON.stringify({ count: 42, flag: true });
    expect(extractArgsPreview(json)).toBe(json);
  });

  test("skips empty string values", () => {
    const json = JSON.stringify({ empty: "", path: "/real" });
    expect(extractArgsPreview(json)).toBe("/real");
  });

  test("handles array JSON by falling back to raw", () => {
    const json = JSON.stringify(["a", "b"]);
    expect(extractArgsPreview(json)).toBe(json);
  });
});

describe("extractProjectName", () => {
  test("returns null for null input", () => {
    expect(extractProjectName(null)).toBeNull();
  });

  test("extracts last segment from path", () => {
    expect(extractProjectName("/home/user/projects/myremote")).toBe("myremote");
  });

  test("handles trailing slash", () => {
    expect(extractProjectName("/home/user/projects/myremote/")).toBe("myremote");
  });

  test("handles multiple trailing slashes", () => {
    expect(extractProjectName("/home/user/projects/myremote///")).toBe("myremote");
  });

  test("handles single segment", () => {
    expect(extractProjectName("myremote")).toBe("myremote");
  });

  test("returns null for empty string", () => {
    expect(extractProjectName("")).toBeNull();
  });

  test("returns null for slash only", () => {
    expect(extractProjectName("/")).toBeNull();
  });
});

describe("resolveSessionName", () => {
  beforeEach(() => {
    clearSessionNameCache();
    vi.clearAllMocks();
  });

  test("fetches and caches session name", async () => {
    (api.sessions.get as ReturnType<typeof vi.fn>).mockResolvedValue({
      id: "s1",
      name: "elegant-snacking",
    });
    const name = await resolveSessionName("s1");
    expect(name).toBe("elegant-snacking");
    expect(api.sessions.get).toHaveBeenCalledWith("s1");
  });

  test("returns cached value on second call without re-fetching", async () => {
    (api.sessions.get as ReturnType<typeof vi.fn>).mockResolvedValue({
      id: "s1",
      name: "elegant-snacking",
    });
    await resolveSessionName("s1");
    const name = await resolveSessionName("s1");
    expect(name).toBe("elegant-snacking");
    expect(api.sessions.get).toHaveBeenCalledTimes(1);
  });

  test("caches null on error", async () => {
    (api.sessions.get as ReturnType<typeof vi.fn>).mockRejectedValue(
      new Error("not found"),
    );
    const name = await resolveSessionName("s1");
    expect(name).toBeNull();
    // Second call returns cached null without fetching
    const name2 = await resolveSessionName("s1");
    expect(name2).toBeNull();
    expect(api.sessions.get).toHaveBeenCalledTimes(1);
  });

  test("handles session with null name", async () => {
    (api.sessions.get as ReturnType<typeof vi.fn>).mockResolvedValue({
      id: "s1",
      name: null,
    });
    const name = await resolveSessionName("s1");
    expect(name).toBeNull();
  });
});

describe("getCachedSessionName", () => {
  beforeEach(() => {
    clearSessionNameCache();
    vi.clearAllMocks();
  });

  test("returns null for unknown session", () => {
    expect(getCachedSessionName("unknown")).toBeNull();
  });

  test("returns cached value after resolve", async () => {
    (api.sessions.get as ReturnType<typeof vi.fn>).mockResolvedValue({
      id: "s1",
      name: "my-session",
    });
    await resolveSessionName("s1");
    expect(getCachedSessionName("s1")).toBe("my-session");
  });
});
