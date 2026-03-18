import { describe, test, expect } from "vitest";
import { extractArgsPreview } from "./notification-utils";

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
