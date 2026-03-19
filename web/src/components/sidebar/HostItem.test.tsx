import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { HostItem } from "./HostItem";
import type { Host, Session, Project } from "../../lib/api";

let mockSessions: Session[] = [];
let mockProjects: Project[] = [];

vi.mock("../../hooks/useSessions", () => ({
  useSessions: () => ({ sessions: mockSessions, loading: false }),
}));

vi.mock("../../hooks/useProjects", () => ({
  useProjects: () => ({ projects: mockProjects, loading: false }),
}));

vi.mock("../../hooks/useAgenticLoops", () => ({
  useAgenticLoops: () => ({ loops: [], loading: false }),
}));

vi.mock("../../stores/claude-task-store", () => {
  const storeState = {
    sessionTaskIndex: new Map(),
    tasks: new Map(),
    fetchTasks: vi.fn().mockResolvedValue(undefined),
  };
  const useClaudeTaskStore = (selector: (s: typeof storeState) => unknown) =>
    selector(storeState);
  useClaudeTaskStore.getState = () => storeState;
  return { useClaudeTaskStore };
});

vi.mock("../../stores/knowledge-store", () => ({
  useKnowledgeStore: (
    selector: (s: { statusByProject: Record<string, unknown> }) => unknown,
  ) => selector({ statusByProject: {} }),
}));

const mockHost: Host = {
  id: "host-1",
  hostname: "my-server",
  status: "online",
  agent_version: "0.1.0",
  os: "linux",
  arch: "x86_64",
  last_seen: new Date().toISOString(),
  connected_at: new Date().toISOString(),
};

const mockSession = (overrides: Partial<Session> = {}): Session => ({
  id: "sess-1",
  host_id: "host-1",
  name: "dev-session",
  shell: "/bin/zsh",
  status: "active",
  cols: 80,
  rows: 24,
  created_at: new Date().toISOString(),
  closed_at: null,
  exit_code: null,
  working_dir: "/home/user",
  project_id: null,
  ...overrides,
});

const mockProject = (overrides: Partial<Project> = {}): Project => ({
  id: "proj-1",
  host_id: "host-1",
  path: "/home/user/my-app",
  name: "my-app",
  has_claude_config: true,
  has_zremote_config: false,
  project_type: "node",
  created_at: new Date().toISOString(),
  parent_project_id: null,
  git_branch: "main",
  git_commit_hash: "abc123",
  git_commit_message: "Initial commit",
  git_is_dirty: false,
  git_ahead: 0,
  git_behind: 0,
  git_remotes: null,
  git_updated_at: null,
  ...overrides,
});

describe("HostItem", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    localStorage.clear();
    mockSessions = [];
    mockProjects = [];
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({}),
      text: async () => "{}",
    });
  });

  test("renders hostname", () => {
    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );
    expect(screen.getByText("my-server")).toBeInTheDocument();
  });

  test("renders expand/collapse button", () => {
    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );
    expect(screen.getByLabelText("Expand")).toBeInTheDocument();
  });

  test("toggles expanded state on click", async () => {
    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );
    const toggleBtn = screen.getByLabelText("Expand");
    await userEvent.click(toggleBtn);
    expect(screen.getByLabelText("Collapse")).toBeInTheDocument();
  });

  test("renders offline host", () => {
    const offlineHost = { ...mockHost, status: "offline" as const };
    render(
      <MemoryRouter>
        <HostItem host={offlineHost} />
      </MemoryRouter>,
    );
    expect(screen.getByText("my-server")).toBeInTheDocument();
  });

  test("shows active session count when sessions exist", () => {
    mockSessions = [
      mockSession({ id: "s1", status: "active" }),
      mockSession({ id: "s2", status: "active" }),
      mockSession({ id: "s3", status: "closed" }),
    ];

    // Need to start expanded to load sessions
    localStorage.setItem("zremote:host-expanded:host-1", "true");

    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );
    // Active sessions count (non-closed) = 2
    expect(screen.getByText("2")).toBeInTheDocument();
  });

  test("shows projects section when expanded with projects", async () => {
    mockProjects = [mockProject()];

    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );

    await userEvent.click(screen.getByLabelText("Expand"));

    expect(screen.getByText("Projects")).toBeInTheDocument();
  });

  test("shows sessions section for orphan sessions when expanded", async () => {
    mockSessions = [mockSession({ project_id: null })];
    mockProjects = [mockProject()];

    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );

    await userEvent.click(screen.getByLabelText("Expand"));

    expect(screen.getByText("Sessions")).toBeInTheDocument();
  });

  test("persists expanded state to localStorage", async () => {
    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );

    await userEvent.click(screen.getByLabelText("Expand"));
    expect(localStorage.getItem("zremote:host-expanded:host-1")).toBe("true");

    await userEvent.click(screen.getByLabelText("Collapse"));
    expect(localStorage.getItem("zremote:host-expanded:host-1")).toBe("false");
  });

  test("restores expanded state from localStorage", () => {
    localStorage.setItem("zremote:host-expanded:host-1", "true");

    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );

    expect(screen.getByLabelText("Collapse")).toBeInTheDocument();
  });

  test("renders New session button", () => {
    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );
    expect(screen.getByLabelText("New session")).toBeInTheDocument();
  });

  test("does not show session count for 0 active sessions", () => {
    mockSessions = [mockSession({ status: "closed" })];
    localStorage.setItem("zremote:host-expanded:host-1", "true");

    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );

    // Should not find a standalone number in the host row
    const hostRow = screen.getByText("my-server").closest("div");
    expect(hostRow?.textContent).not.toContain("1");
  });

  test("shows scan projects button when expanded with projects", async () => {
    mockProjects = [mockProject()];

    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );

    await userEvent.click(screen.getByLabelText("Expand"));

    expect(screen.getByTitle("Scan for projects")).toBeInTheDocument();
  });

  test("does not show orphan Sessions label when no projects exist", async () => {
    mockSessions = [mockSession({ project_id: null })];
    mockProjects = [];

    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );

    await userEvent.click(screen.getByLabelText("Expand"));

    // "Sessions" label should NOT appear because projects.length === 0
    expect(screen.queryByText("Sessions")).not.toBeInTheDocument();
  });

  test("shows suspended session count in active count", () => {
    mockSessions = [
      mockSession({ id: "s1", status: "active" }),
      mockSession({ id: "s2", status: "suspended" }),
    ];

    localStorage.setItem("zremote:host-expanded:host-1", "true");

    render(
      <MemoryRouter>
        <HostItem host={mockHost} />
      </MemoryRouter>,
    );

    expect(screen.getByText("2")).toBeInTheDocument();
  });
});
