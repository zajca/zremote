import { describe, expect, test } from "vitest";
import { detectMissingInputs } from "./action-utils";

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
