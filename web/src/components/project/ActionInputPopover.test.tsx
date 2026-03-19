import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { ActionInputPopover } from "./ActionInputPopover";
import type { Project } from "../../lib/api";

const mockWorktrees: Project[] = [
  {
    id: "wt-1",
    host_id: "host-1",
    path: "/code/project/wt-feature",
    name: "wt-feature",
    has_claude_config: false,
    has_zremote_config: false,
    project_type: "rust",
    created_at: "2026-01-01T00:00:00Z",
    parent_project_id: "proj-1",
    git_branch: "feature/login",
    git_commit_hash: "abc123",
    git_commit_message: "wip",
    git_is_dirty: false,
    git_ahead: 0,
    git_behind: 0,
    git_remotes: null,
    git_updated_at: null,
  },
  {
    id: "wt-2",
    host_id: "host-1",
    path: "/code/project/wt-bugfix",
    name: "wt-bugfix",
    has_claude_config: false,
    has_zremote_config: false,
    project_type: "rust",
    created_at: "2026-01-02T00:00:00Z",
    parent_project_id: "proj-1",
    git_branch: "fix/crash",
    git_commit_hash: "def456",
    git_commit_message: "fix",
    git_is_dirty: false,
    git_ahead: 1,
    git_behind: 0,
    git_remotes: null,
    git_updated_at: null,
  },
];

vi.mock("../../lib/api", async () => {
  const actual = await vi.importActual<Record<string, unknown>>("../../lib/api");
  const actualApi = actual.api as Record<string, unknown>;
  return {
    ...actual,
    api: {
      ...actualApi,
      projects: {
        ...(actualApi.projects as Record<string, unknown>),
        worktrees: vi.fn(),
      },
    },
  };
});

async function getWorktreesMock() {
  const { api } = await import("../../lib/api");
  return api.projects.worktrees as ReturnType<typeof vi.fn>;
}

beforeEach(() => {
  vi.restoreAllMocks();
});

describe("ActionInputPopover", () => {
  test("renders worktree dropdown when needsWorktree=true", async () => {
    const mock = await getWorktreesMock();
    mock.mockResolvedValue(mockWorktrees);
    const onSubmit = vi.fn();
    const onCancel = vi.fn();

    render(
      <ActionInputPopover
        projectId="proj-1"
        needsWorktree
        needsBranch={false}
        onSubmit={onSubmit}
        onCancel={onCancel}
      />,
    );

    await waitFor(() => {
      expect(screen.getByLabelText("Worktree")).toBeInTheDocument();
    });

    const select = screen.getByLabelText("Worktree") as HTMLSelectElement;
    expect(select.options).toHaveLength(2);
    expect(select.options[0]!.textContent).toBe("feature/login");
    expect(select.options[1]!.textContent).toBe("fix/crash");
  });

  test("renders branch input when needsBranch=true", () => {
    const onSubmit = vi.fn();
    const onCancel = vi.fn();

    render(
      <ActionInputPopover
        projectId="proj-1"
        needsWorktree={false}
        needsBranch
        onSubmit={onSubmit}
        onCancel={onCancel}
      />,
    );

    expect(screen.getByLabelText("Branch")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("e.g. feature/my-branch")).toBeInTheDocument();
  });

  test("calls onSubmit with selected worktree values", async () => {
    const user = userEvent.setup();
    const mock = await getWorktreesMock();
    mock.mockResolvedValue(mockWorktrees);
    const onSubmit = vi.fn();
    const onCancel = vi.fn();

    render(
      <ActionInputPopover
        projectId="proj-1"
        needsWorktree
        needsBranch={false}
        onSubmit={onSubmit}
        onCancel={onCancel}
      />,
    );

    await waitFor(() => {
      expect(screen.getByLabelText("Worktree")).toBeInTheDocument();
    });

    await user.selectOptions(screen.getByLabelText("Worktree"), "/code/project/wt-bugfix");
    await user.click(screen.getByText("Run"));

    expect(onSubmit).toHaveBeenCalledWith({
      worktreePath: "/code/project/wt-bugfix",
      branch: "fix/crash",
    });
  });

  test("calls onCancel on Escape", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    const onCancel = vi.fn();

    render(
      <ActionInputPopover
        projectId="proj-1"
        needsWorktree={false}
        needsBranch
        onSubmit={onSubmit}
        onCancel={onCancel}
      />,
    );

    await user.keyboard("{Escape}");
    expect(onCancel).toHaveBeenCalled();
  });

  test("shows empty state when no worktrees", async () => {
    const mock = await getWorktreesMock();
    mock.mockResolvedValue([]);
    const onSubmit = vi.fn();
    const onCancel = vi.fn();

    render(
      <ActionInputPopover
        projectId="proj-1"
        needsWorktree
        needsBranch={false}
        onSubmit={onSubmit}
        onCancel={onCancel}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("popover-empty")).toBeInTheDocument();
    });

    expect(screen.getByText("No worktrees found")).toBeInTheDocument();
    expect(screen.queryByText("Run")).not.toBeInTheDocument();
  });

  test("shows loading state while fetching", async () => {
    const mock = await getWorktreesMock();
    mock.mockReturnValue(new Promise(() => {})); // never resolves

    render(
      <ActionInputPopover
        projectId="proj-1"
        needsWorktree
        needsBranch={false}
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByTestId("popover-loading")).toBeInTheDocument();
    expect(screen.getByText("Loading worktrees...")).toBeInTheDocument();
  });
});
