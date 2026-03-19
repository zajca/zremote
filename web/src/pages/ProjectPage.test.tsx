import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";
import { ProjectPage } from "./ProjectPage";

const mockNavigate = vi.fn();
vi.mock("react-router", async () => {
  const actual = await vi.importActual("react-router");
  return {
    ...actual,
    useNavigate: () => mockNavigate,
  };
});

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

vi.mock("../components/layout/Toast", () => ({
  showToast: vi.fn(),
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
  mockNavigate.mockReset();
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
        <Route path="/" element={<div>Home</div>} />
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
      expect(screen.getByText("actions")).toBeInTheDocument();
      expect(screen.getByText("loops")).toBeInTheDocument();
      expect(screen.getByText("knowledge")).toBeInTheDocument();
      expect(screen.getByText("settings")).toBeInTheDocument();
    });
  });

  test("renders configure button in header", async () => {
    renderProjectPage();
    await waitFor(() => {
      expect(screen.getByText("Configure")).toBeInTheDocument();
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

  test("handleDelete calls api and navigates on success", async () => {
    const user = userEvent.setup();
    window.confirm = vi.fn().mockReturnValue(true);
    renderProjectPage();

    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });

    await user.click(screen.getByText("Remove"));

    await waitFor(() => {
      expect(window.confirm).toHaveBeenCalledWith(
        'Remove project "my-project" from tracking?',
      );
      expect(global.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/api/projects/proj-1"),
        expect.objectContaining({ method: "DELETE" }),
      );
      expect(mockNavigate).toHaveBeenCalledWith("/");
    });
  });

  test("handleDelete does nothing when confirm cancelled", async () => {
    const user = userEvent.setup();
    window.confirm = vi.fn().mockReturnValue(false);
    renderProjectPage();

    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });

    const fetchCallsBefore = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.length;
    await user.click(screen.getByText("Remove"));

    expect(window.confirm).toHaveBeenCalled();
    // No additional DELETE fetch should have been made
    const fetchCallsAfter = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.length;
    const deleteCalls = (global.fetch as ReturnType<typeof vi.fn>).mock.calls
      .slice(fetchCallsBefore)
      .filter((c: unknown[]) => (c[1] as Record<string, string>)?.method === "DELETE");
    expect(deleteCalls).toHaveLength(0);
    expect(mockNavigate).not.toHaveBeenCalled();
  });

  test("handleOpenTerminal creates session and navigates", async () => {
    const user = userEvent.setup();
    const sessionResponse = { id: "sess-new", status: "active" };
    global.fetch = vi.fn().mockImplementation((url: string, opts?: RequestInit) => {
      if (url.includes("/api/projects/proj-1/sessions")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/projects/proj-1")) {
        return mockFetchResponse(mockProject);
      }
      if (url.includes("/api/hosts/host-1/sessions") && opts?.method === "POST") {
        return mockFetchResponse(sessionResponse);
      }
      return mockFetchResponse({});
    });

    renderProjectPage();

    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });

    await user.click(screen.getByText("Terminal"));

    await waitFor(() => {
      expect(global.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/api/hosts/host-1/sessions"),
        expect.objectContaining({ method: "POST" }),
      );
      expect(mockNavigate).toHaveBeenCalledWith(
        "/hosts/host-1/sessions/sess-new",
      );
    });
  });

  test("handleConfigureWithClaude calls api and navigates", async () => {
    const user = userEvent.setup();
    const taskResponse = { session_id: "sess-cfg", id: "task-1" };
    global.fetch = vi.fn().mockImplementation((url: string, opts?: RequestInit) => {
      if (url.includes("/api/projects/proj-1/sessions")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/projects/proj-1/configure") && opts?.method === "POST") {
        return mockFetchResponse(taskResponse);
      }
      if (url.includes("/api/projects/proj-1")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    renderProjectPage();

    await waitFor(() => {
      expect(screen.getByText("Configure")).toBeInTheDocument();
    });

    await user.click(screen.getByText("Configure"));

    await waitFor(() => {
      expect(global.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/api/projects/proj-1/configure"),
        expect.objectContaining({ method: "POST" }),
      );
      expect(mockNavigate).toHaveBeenCalledWith(
        "/hosts/host-1/sessions/sess-cfg",
      );
    });
  });

  test("handleRefreshGit refreshes git status on git tab", async () => {
    const user = userEvent.setup();
    const { showToast } = await import("../components/layout/Toast");

    global.fetch = vi.fn().mockImplementation((url: string, opts?: RequestInit) => {
      if (url.includes("/api/projects/proj-1/sessions")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/projects/proj-1/git/refresh") && opts?.method === "POST") {
        return mockFetchResponse({});
      }
      if (url.includes("/api/projects/proj-1/worktrees")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/projects/proj-1/actions")) {
        return mockFetchResponse({ actions: [] });
      }
      if (url.includes("/api/projects/proj-1")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    renderProjectPage();

    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });

    // Switch to git tab
    await user.click(screen.getByText("git"));

    await waitFor(() => {
      expect(screen.getByText("Refresh")).toBeInTheDocument();
    });

    await user.click(screen.getByText("Refresh"));

    await waitFor(() => {
      expect(global.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/api/projects/proj-1/git/refresh"),
        expect.objectContaining({ method: "POST" }),
      );
      expect(showToast).toHaveBeenCalledWith("Git status refreshed", "success");
    });
  });

  test("git tab shows branch and commit info", async () => {
    const user = userEvent.setup();

    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/api/projects/proj-1/sessions")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/projects/proj-1/worktrees")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/projects/proj-1/actions")) {
        return mockFetchResponse({ actions: [] });
      }
      if (url.includes("/api/projects/proj-1")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    renderProjectPage();

    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });

    await user.click(screen.getByText("git"));

    await waitFor(() => {
      expect(screen.getByText("main")).toBeInTheDocument();
      expect(screen.getByText("abc123def456")).toBeInTheDocument();
      expect(screen.getByText("Initial commit")).toBeInTheDocument();
      expect(screen.getByText("clean")).toBeInTheDocument();
    });
  });

  test("git tab shows remotes when available", async () => {
    const user = userEvent.setup();
    const projectWithRemotes = {
      ...mockProject,
      git_remotes: JSON.stringify([
        { name: "origin", url: "https://github.com/user/repo.git" },
      ]),
    };

    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/api/projects/proj-1/sessions")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/projects/proj-1/worktrees")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/projects/proj-1/actions")) {
        return mockFetchResponse({ actions: [] });
      }
      if (url.includes("/api/projects/proj-1")) {
        return mockFetchResponse(projectWithRemotes);
      }
      return mockFetchResponse({});
    });

    renderProjectPage();

    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });

    await user.click(screen.getByText("git"));

    await waitFor(() => {
      expect(screen.getByText("Remotes")).toBeInTheDocument();
      expect(screen.getByText("origin")).toBeInTheDocument();
      expect(
        screen.getByText("https://github.com/user/repo.git"),
      ).toBeInTheDocument();
    });
  });

  test("handleCreateWorktree creates worktree and shows toast", async () => {
    const user = userEvent.setup();
    const { showToast } = await import("../components/layout/Toast");

    global.fetch = vi.fn().mockImplementation((url: string, opts?: RequestInit) => {
      if (url.includes("/api/projects/proj-1/sessions")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/projects/proj-1/worktrees") && opts?.method === "POST") {
        return mockFetchResponse({});
      }
      if (url.includes("/api/projects/proj-1/worktrees")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/projects/proj-1/actions")) {
        return mockFetchResponse({ actions: [] });
      }
      if (url.includes("/api/projects/proj-1")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    renderProjectPage();

    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });

    // Switch to git tab
    await user.click(screen.getByText("git"));

    await waitFor(() => {
      expect(screen.getByText("Create Worktree")).toBeInTheDocument();
    });

    // Open create worktree form
    await user.click(screen.getByText("Create Worktree"));

    await waitFor(() => {
      expect(screen.getByPlaceholderText("feature/my-branch")).toBeInTheDocument();
    });

    // Fill in branch name
    await user.type(screen.getByPlaceholderText("feature/my-branch"), "feature/new");

    // Click create button
    const createButtons = screen.getAllByText("Create");
    // The last "Create" button is the submit button in the form
    const submitButton = createButtons[createButtons.length - 1];
    await user.click(submitButton);

    await waitFor(() => {
      expect(global.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/api/projects/proj-1/worktrees"),
        expect.objectContaining({ method: "POST" }),
      );
      expect(showToast).toHaveBeenCalledWith("Worktree created", "success");
    });
  });

  test("handleDeleteWorktree deletes worktree on confirm", async () => {
    const user = userEvent.setup();
    window.confirm = vi.fn().mockReturnValue(true);
    const { showToast } = await import("../components/layout/Toast");

    const worktreeProject = {
      ...mockProject,
      id: "wt-1",
      name: "feature-wt",
      path: "/home/user/my-project-feature",
      parent_project_id: "proj-1",
      git_branch: "feature/delete-test",
    };

    global.fetch = vi.fn().mockImplementation((url: string, opts?: RequestInit) => {
      if (url.includes("/api/projects/proj-1/sessions")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/projects/proj-1/worktrees/wt-1") && opts?.method === "DELETE") {
        return mockFetchResponse({});
      }
      if (url.includes("/api/projects/proj-1/worktrees")) {
        return mockFetchResponse([worktreeProject]);
      }
      if (url.includes("/api/projects/proj-1/actions")) {
        return mockFetchResponse({ actions: [] });
      }
      if (url.includes("/api/projects/proj-1")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    renderProjectPage();

    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });

    // Switch to git tab
    await user.click(screen.getByText("git"));

    // Wait for worktrees to load (WorktreeCard shows git_branch ?? name)
    await waitFor(() => {
      expect(screen.getByText("feature/delete-test")).toBeInTheDocument();
    });

    // Click the delete button on the worktree card (it has aria-label)
    const deleteButton = screen.getByLabelText("Delete worktree");
    await user.click(deleteButton);

    await waitFor(() => {
      expect(window.confirm).toHaveBeenCalledWith(
        'Delete worktree "feature-wt"?',
      );
      expect(global.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/api/projects/proj-1/worktrees/wt-1"),
        expect.objectContaining({ method: "DELETE" }),
      );
      expect(showToast).toHaveBeenCalledWith("Worktree deleted", "success");
    });
  });

  test("handleOpenWorktreeTerminal opens terminal in worktree dir", async () => {
    const user = userEvent.setup();

    const worktreeProject = {
      ...mockProject,
      id: "wt-1",
      name: "feature-wt",
      path: "/home/user/my-project-feature",
      parent_project_id: "proj-1",
      git_branch: "feature/term-test",
    };

    global.fetch = vi.fn().mockImplementation((url: string, opts?: RequestInit) => {
      if (url.includes("/api/projects/proj-1/sessions")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/hosts/host-1/sessions") && opts?.method === "POST") {
        return mockFetchResponse({ id: "sess-wt", status: "active" });
      }
      if (url.includes("/api/projects/proj-1/worktrees")) {
        return mockFetchResponse([worktreeProject]);
      }
      if (url.includes("/api/projects/proj-1/actions")) {
        return mockFetchResponse({ actions: [] });
      }
      if (url.includes("/api/projects/proj-1")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    renderProjectPage();

    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });

    // Switch to git tab
    await user.click(screen.getByText("git"));

    await waitFor(() => {
      expect(screen.getByText("feature/term-test")).toBeInTheDocument();
    });

    // Click the terminal button on worktree card
    const terminalButton = screen.getByLabelText("Open terminal");
    await user.click(terminalButton);

    await waitFor(() => {
      expect(global.fetch).toHaveBeenCalledWith(
        expect.stringContaining("/api/hosts/host-1/sessions"),
        expect.objectContaining({ method: "POST" }),
      );
      expect(mockNavigate).toHaveBeenCalledWith(
        "/hosts/host-1/sessions/sess-wt",
      );
    });
  });

  test("handleConfigureWithClaude shows error toast on failure", async () => {
    const user = userEvent.setup();
    const { showToast } = await import("../components/layout/Toast");

    global.fetch = vi.fn().mockImplementation((url: string, opts?: RequestInit) => {
      if (url.includes("/api/projects/proj-1/sessions")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/projects/proj-1/configure") && opts?.method === "POST") {
        return mockFetchError();
      }
      if (url.includes("/api/projects/proj-1")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    renderProjectPage();

    await waitFor(() => {
      expect(screen.getByText("Configure")).toBeInTheDocument();
    });

    await user.click(screen.getByText("Configure"));

    await waitFor(() => {
      expect(showToast).toHaveBeenCalledWith(
        "Failed to start configuration",
        "error",
      );
    });
  });

  test("handleOpenTerminal shows error toast on failure", async () => {
    const user = userEvent.setup();
    const { showToast } = await import("../components/layout/Toast");

    global.fetch = vi.fn().mockImplementation((url: string, opts?: RequestInit) => {
      if (url.includes("/api/projects/proj-1/sessions")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/api/hosts/host-1/sessions") && opts?.method === "POST") {
        return mockFetchError();
      }
      if (url.includes("/api/projects/proj-1")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    renderProjectPage();

    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });

    await user.click(screen.getByText("Terminal"));

    await waitFor(() => {
      expect(showToast).toHaveBeenCalledWith(
        "Failed to open terminal",
        "error",
      );
    });
  });
});
