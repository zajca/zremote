import { describe, expect, test } from "vitest";
import type { ProjectAction } from "../../lib/api";
import { detectMissingInputs, effectiveScopes, hasScope } from "./action-utils";

function makeAction(overrides: Partial<ProjectAction> = {}): ProjectAction {
  return {
    name: "test",
    command: "echo test",
    env: {},
    worktree_scoped: false,
    ...overrides,
  };
}

describe("detectMissingInputs", () => {
  test("detects worktree_path template without props", () => {
    const result = detectMissingInputs(
      "cd {{worktree_path}} && cargo test",
      undefined,
      undefined,
      undefined,
    );
    expect(result).toEqual({ needsWorktree: true, needsBranch: false });
  });

  test("detects worktree_name template without props", () => {
    const result = detectMissingInputs(
      "echo {{worktree_name}}",
      undefined,
      undefined,
      undefined,
    );
    expect(result).toEqual({ needsWorktree: true, needsBranch: false });
  });

  test("detects branch template without props", () => {
    const result = detectMissingInputs(
      "git checkout {{branch}}",
      undefined,
      undefined,
      undefined,
    );
    expect(result).toEqual({ needsWorktree: false, needsBranch: true });
  });

  test("returns no missing inputs when no templates", () => {
    const result = detectMissingInputs("cargo test", undefined, undefined, undefined);
    expect(result).toEqual({ needsWorktree: false, needsBranch: false });
  });

  test("returns no missing when worktreePath prop provided", () => {
    const result = detectMissingInputs(
      "cd {{worktree_path}} && cargo test",
      undefined,
      "/some/path",
      "main",
    );
    expect(result).toEqual({ needsWorktree: false, needsBranch: false });
  });

  test("detects templates in working_dir", () => {
    const result = detectMissingInputs(
      "cargo test",
      "{{worktree_path}}",
      undefined,
      undefined,
    );
    expect(result).toEqual({ needsWorktree: true, needsBranch: false });
  });
});

describe("effectiveScopes", () => {
  test("returns explicit scopes when present", () => {
    const action = makeAction({ scopes: ["sidebar", "project"] });
    expect(effectiveScopes(action)).toEqual(["sidebar", "project"]);
  });

  test("derives project + command_palette for non-worktree legacy action", () => {
    const action = makeAction({ worktree_scoped: false });
    expect(effectiveScopes(action)).toEqual(["project", "command_palette"]);
  });

  test("derives worktree + command_palette for worktree legacy action", () => {
    const action = makeAction({ worktree_scoped: true });
    expect(effectiveScopes(action)).toEqual(["worktree", "command_palette"]);
  });

  test("explicit scopes override worktree_scoped", () => {
    const action = makeAction({ worktree_scoped: true, scopes: ["sidebar"] });
    expect(effectiveScopes(action)).toEqual(["sidebar"]);
  });

  test("empty scopes array falls back to legacy", () => {
    const action = makeAction({ scopes: [] });
    expect(effectiveScopes(action)).toEqual(["project", "command_palette"]);
  });
});

describe("hasScope", () => {
  test("returns true for matching scope", () => {
    const action = makeAction({ scopes: ["project", "sidebar"] });
    expect(hasScope(action, "project")).toBe(true);
    expect(hasScope(action, "sidebar")).toBe(true);
  });

  test("returns false for non-matching scope", () => {
    const action = makeAction({ scopes: ["project"] });
    expect(hasScope(action, "worktree")).toBe(false);
    expect(hasScope(action, "command_palette")).toBe(false);
  });

  test("works with legacy fallback", () => {
    const action = makeAction({ worktree_scoped: false });
    expect(hasScope(action, "project")).toBe(true);
    expect(hasScope(action, "command_palette")).toBe(true);
    expect(hasScope(action, "sidebar")).toBe(false);
  });
});
