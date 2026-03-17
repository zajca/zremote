import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { WorktreeCard } from "./WorktreeCard";
import type { Project, ProjectAction } from "../../lib/api";

beforeEach(() => {
  vi.restoreAllMocks();
});

const mockWorktree: Project = {
  id: "wt-1",
  host_id: "host-1",
  path: "/home/user/project/.worktrees/feature-x",
  name: "feature-x",
  has_claude_config: false,
  project_type: "rust",
  created_at: new Date().toISOString(),
  parent_project_id: "proj-1",
  git_branch: "feature-x",
  git_commit_hash: "abc123def456789",
  git_commit_message: "Add feature X",
  git_is_dirty: false,
  git_ahead: 0,
  git_behind: 0,
  git_remotes: null,
  git_updated_at: null,
  has_zremote_config: false,
};

const worktreeAction: ProjectAction = {
  name: "setup",
  command: "bun install",
  env: {},
  worktree_scoped: true,
};

function renderCard(
  worktree: Project = mockWorktree,
  actions: ProjectAction[] = [],
) {
  return render(
    <MemoryRouter>
      <WorktreeCard
        worktree={worktree}
        parentProjectId="proj-1"
        hostId="host-1"
        worktreeActions={actions}
        onDelete={vi.fn()}
        onOpenTerminal={vi.fn()}
      />
    </MemoryRouter>,
  );
}

describe("WorktreeCard", () => {
  test("renders branch and commit info", () => {
    renderCard();
    expect(screen.getByText("feature-x")).toBeInTheDocument();
    expect(screen.getByText("abc123d")).toBeInTheDocument();
    expect(screen.getByText("Add feature X")).toBeInTheDocument();
  });

  test("shows dirty indicator when dirty", () => {
    renderCard({ ...mockWorktree, git_is_dirty: true });
    expect(screen.getByText("Modified")).toBeInTheDocument();
  });

  test("does not show dirty indicator when clean", () => {
    renderCard();
    expect(screen.queryByText("Modified")).not.toBeInTheDocument();
  });

  test("renders action buttons", () => {
    renderCard();
    expect(screen.getByLabelText("Open terminal")).toBeInTheDocument();
    expect(screen.getByLabelText("Delete worktree")).toBeInTheDocument();
  });

  test("renders worktree path", () => {
    renderCard();
    expect(
      screen.getByText("/home/user/project/.worktrees/feature-x"),
    ).toBeInTheDocument();
  });

  test("renders worktree actions when provided", () => {
    renderCard(mockWorktree, [worktreeAction]);
    expect(screen.getByText("setup")).toBeInTheDocument();
    expect(screen.getByText("bun install")).toBeInTheDocument();
  });
});
