import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { ProjectItem } from "./ProjectItem";
import type { Project } from "../../lib/api";

vi.mock("../../stores/claude-task-store", () => ({
  useClaudeTaskStore: Object.assign(
    (selector: (s: { tasks: Map<string, unknown>; sessionTaskIndex: Map<string, string> }) => unknown) =>
      selector({ tasks: new Map(), sessionTaskIndex: new Map() }),
    {
      getState: () => ({ fetchTasks: vi.fn() }),
    },
  ),
}));

vi.mock("../../stores/knowledge-store", () => ({
  useKnowledgeStore: (selector: (s: { statusByProject: Record<string, unknown> }) => unknown) =>
    selector({ statusByProject: {} }),
}));

const mockProject: Project = {
  id: "proj-1",
  host_id: "host-1",
  name: "my-project",
  path: "/home/user/my-project",
  project_type: "rust",
  has_claude_config: true,
  has_zremote_config: false,
  git_branch: "main",
  git_commit_hash: "abc123def456",
  git_commit_message: "Initial commit",
  git_is_dirty: false,
  git_ahead: 0,
  git_behind: 0,
  git_remotes: null,
  parent_project_id: null,
  git_updated_at: null,
  created_at: new Date().toISOString(),
};

describe("ProjectItem", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ([]),
    });
  });

  test("renders project name", () => {
    render(
      <MemoryRouter>
        <ProjectItem project={mockProject} sessions={[]} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("my-project")).toBeInTheDocument();
  });

  test("renders git branch icon with tooltip", () => {
    render(
      <MemoryRouter>
        <ProjectItem project={mockProject} sessions={[]} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("Branch: main")).toBeInTheDocument();
  });

  test("renders claude config icon when has_claude_config", () => {
    render(
      <MemoryRouter>
        <ProjectItem project={mockProject} sessions={[]} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("Claude Code config (.claude/)")).toBeInTheDocument();
  });

  test("does not render claude config icon when config is absent", () => {
    const noConfig = { ...mockProject, has_claude_config: false };
    render(
      <MemoryRouter>
        <ProjectItem project={noConfig} sessions={[]} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.queryByText("Claude Code config (.claude/)")).not.toBeInTheDocument();
  });

  test("renders zremote config icon when has_zremote_config", () => {
    const withZremote = { ...mockProject, has_zremote_config: true };
    render(
      <MemoryRouter>
        <ProjectItem project={withZremote} sessions={[]} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("ZRemote config (.zremote/)")).toBeInTheDocument();
  });

  test("does not render zremote config icon when config is absent", () => {
    render(
      <MemoryRouter>
        <ProjectItem project={mockProject} sessions={[]} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.queryByText("ZRemote config (.zremote/)")).not.toBeInTheDocument();
  });

  test("renders dirty indicator in branch tooltip when git is dirty", () => {
    const dirtyProject = { ...mockProject, git_is_dirty: true };
    render(
      <MemoryRouter>
        <ProjectItem project={dirtyProject} sessions={[]} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByText("Branch: main (uncommitted changes)")).toBeInTheDocument();
  });

  test("renders Start Claude button", () => {
    render(
      <MemoryRouter>
        <ProjectItem project={mockProject} sessions={[]} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByLabelText("Start Claude in project")).toBeInTheDocument();
  });

  test("renders New session button", () => {
    render(
      <MemoryRouter>
        <ProjectItem project={mockProject} sessions={[]} hostId="host-1" />
      </MemoryRouter>,
    );
    expect(screen.getByLabelText("New session in project")).toBeInTheDocument();
  });
});
