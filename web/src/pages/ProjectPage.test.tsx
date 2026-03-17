import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";
import { ProjectPage } from "./ProjectPage";

vi.mock("../hooks/useAgenticLoops", () => ({
  useAgenticLoops: () => ({ loops: [], loading: false }),
}));

vi.mock("../stores/claude-task-store", () => ({
  useClaudeTaskStore: Object.assign(
    (selector: (s: { sessionTaskIndex: Map<string, string>; tasks: Map<string, unknown> }) => unknown) =>
      selector({ sessionTaskIndex: new Map(), tasks: new Map() }),
    {
      getState: () => ({ fetchTasks: vi.fn() }),
    },
  ),
}));

vi.mock("../stores/knowledge-store", () => ({
  useKnowledgeStore: (selector: (s: Record<string, unknown>) => unknown) =>
    selector({ statusByProject: {} }),
}));

const mockProject = {
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
  created_at: new Date().toISOString(),
  updated_at: new Date().toISOString(),
};

function mockFetchResponse(data: unknown) {
  return Promise.resolve({
    ok: true,
    text: async () => JSON.stringify(data),
    json: async () => data,
  });
}

function mockFetchError() {
  return Promise.resolve({
    ok: false,
    text: async () => "not found",
    statusText: "Not Found",
    status: 404,
  });
}

beforeEach(() => {
  vi.restoreAllMocks();
  global.fetch = vi.fn().mockImplementation((url: string) => {
    if (url.includes("/api/projects/proj-1/sessions")) {
      return mockFetchResponse([]);
    }
    if (url.includes("/api/projects/proj-1")) {
      return mockFetchResponse(mockProject);
    }
    if (url.includes("/api/loops")) {
      return mockFetchResponse([]);
    }
    if (url.includes("/api/claude-sessions")) {
      return mockFetchResponse([]);
    }
    return mockFetchResponse({});
  });
});

function renderProjectPage() {
  return render(
    <MemoryRouter initialEntries={["/projects/proj-1"]}>
      <Routes>
        <Route path="/projects/:projectId" element={<ProjectPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe("ProjectPage", () => {
  test("shows loading state initially", () => {
    renderProjectPage();
    expect(screen.getByText("Loading project...")).toBeInTheDocument();
  });

  test("renders project name after loading", async () => {
    renderProjectPage();
    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });
  });

  test("renders project type badge", async () => {
    renderProjectPage();
    await waitFor(() => {
      expect(screen.getByText("rust")).toBeInTheDocument();
    });
  });

  test("renders .claude badge", async () => {
    renderProjectPage();
    await waitFor(() => {
      expect(screen.getByText(".claude")).toBeInTheDocument();
    });
  });

  test("renders project path", async () => {
    renderProjectPage();
    await waitFor(() => {
      expect(screen.getByText("/home/user/my-project")).toBeInTheDocument();
    });
  });

  test("renders tab buttons", async () => {
    renderProjectPage();
    await waitFor(() => {
      expect(screen.getByText("sessions")).toBeInTheDocument();
      expect(screen.getByText("loops")).toBeInTheDocument();
      expect(screen.getByText("knowledge")).toBeInTheDocument();
      expect(screen.getByText("settings")).toBeInTheDocument();
    });
  });

  test("shows Project not found for nonexistent project", async () => {
    global.fetch = vi.fn().mockImplementation(() => mockFetchError());
    render(
      <MemoryRouter initialEntries={["/projects/unknown"]}>
        <Routes>
          <Route path="/projects/:projectId" element={<ProjectPage />} />
        </Routes>
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(screen.getByText("Project not found")).toBeInTheDocument();
    });
  });
});
