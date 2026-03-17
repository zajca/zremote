import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { ActionCard } from "./ActionCard";
import type { ProjectAction } from "../../lib/api";

vi.mock("../../lib/api", async () => {
  const actual = await vi.importActual("../../lib/api");
  return { ...actual };
});

beforeEach(() => {
  vi.restoreAllMocks();
});

const baseAction: ProjectAction = {
  name: "test-action",
  command: "cargo test --workspace",
  description: "Run all tests",
  icon: undefined,
  working_dir: undefined,
  env: {},
  worktree_scoped: false,
};

function renderCard(action: ProjectAction = baseAction) {
  return render(
    <MemoryRouter>
      <ActionCard
        action={action}
        projectId="proj-1"
        hostId="host-1"
      />
    </MemoryRouter>,
  );
}

describe("ActionCard", () => {
  test("renders action name and description", () => {
    renderCard();
    expect(screen.getByText("test-action")).toBeInTheDocument();
    expect(screen.getByText("Run all tests")).toBeInTheDocument();
  });

  test("renders command in monospace", () => {
    renderCard();
    const commandEl = screen.getByText("cargo test --workspace");
    expect(commandEl).toBeInTheDocument();
    expect(commandEl.className).toContain("font-mono");
  });

  test("shows worktree badge for worktree_scoped actions", () => {
    renderCard({ ...baseAction, worktree_scoped: true });
    expect(screen.getByText("worktree")).toBeInTheDocument();
  });

  test("does not show worktree badge for project actions", () => {
    renderCard();
    expect(screen.queryByText("worktree")).not.toBeInTheDocument();
  });

  test("renders Run button", () => {
    renderCard();
    expect(screen.getByText("Run")).toBeInTheDocument();
  });

  test("renders without description", () => {
    renderCard({ ...baseAction, description: undefined });
    expect(screen.getByText("test-action")).toBeInTheDocument();
    expect(screen.queryByText("Run all tests")).not.toBeInTheDocument();
  });
});
