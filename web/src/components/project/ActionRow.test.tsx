import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { ActionRow } from "./ActionRow";
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

function renderRow(action: ProjectAction = baseAction) {
  return render(
    <MemoryRouter>
      <ActionRow
        action={action}
        projectId="proj-1"
        hostId="host-1"
      />
    </MemoryRouter>,
  );
}

describe("ActionRow", () => {
  test("renders action name and command", () => {
    renderRow();
    expect(screen.getByText("test-action")).toBeInTheDocument();
    expect(screen.getByText("cargo test --workspace")).toBeInTheDocument();
  });

  test("renders command in monospace", () => {
    renderRow();
    const commandEl = screen.getByText("cargo test --workspace");
    expect(commandEl).toBeInTheDocument();
    expect(commandEl.className).toContain("font-mono");
  });

  test("shows worktree badge for worktree_scoped actions", () => {
    renderRow({ ...baseAction, worktree_scoped: true });
    expect(screen.getByText("worktree")).toBeInTheDocument();
  });

  test("does not show worktree badge for project actions", () => {
    renderRow();
    expect(screen.queryByText("worktree")).not.toBeInTheDocument();
  });

  test("renders run button with aria-label", () => {
    renderRow();
    expect(screen.getByLabelText("Run test-action")).toBeInTheDocument();
  });

  test("does not show description by default (collapsed)", () => {
    renderRow();
    expect(screen.queryByText("Run all tests")).not.toBeInTheDocument();
  });

  test("expands to show description and full command on click", async () => {
    const user = userEvent.setup();
    renderRow();

    await user.click(screen.getByText("cargo test --workspace"));
    expect(screen.getByText("Run all tests")).toBeInTheDocument();
  });

  test("renders without description when expanded", async () => {
    const user = userEvent.setup();
    renderRow({ ...baseAction, description: undefined });

    await user.click(screen.getByText("cargo test --workspace"));
    expect(screen.queryByText("Run all tests")).not.toBeInTheDocument();
  });
});

describe("ActionRow template integration", () => {
  test("shows popover for action with {{worktree_path}} and no props", async () => {
    const user = userEvent.setup();
    render(
      <MemoryRouter>
        <ActionRow
          action={{ ...baseAction, command: "cd {{worktree_path}} && cargo test" }}
          projectId="proj-1"
          hostId="host-1"
        />
      </MemoryRouter>,
    );

    await user.click(screen.getByLabelText("Run test-action"));
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  test("runs immediately for action without template vars", async () => {
    const user = userEvent.setup();
    render(
      <MemoryRouter>
        <ActionRow
          action={baseAction}
          projectId="proj-1"
          hostId="host-1"
        />
      </MemoryRouter>,
    );

    await user.click(screen.getByLabelText("Run test-action"));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  test("runs immediately when worktreePath prop is provided", async () => {
    const user = userEvent.setup();
    render(
      <MemoryRouter>
        <ActionRow
          action={{ ...baseAction, command: "cd {{worktree_path}} && cargo test" }}
          projectId="proj-1"
          hostId="host-1"
          worktreePath="/some/path"
          worktreeBranch="feature/x"
        />
      </MemoryRouter>,
    );

    await user.click(screen.getByLabelText("Run test-action"));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });
});
