import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { LinearIssuesPanel } from "./LinearIssuesPanel";

function mockFetchResponse(data: unknown) {
  return Promise.resolve({
    ok: true,
    text: async () => JSON.stringify(data),
    json: async () => data,
  });
}

const mockSettings = {
  settings: {
    env: {},
    agentic: { auto_detect: true, default_permissions: [], auto_approve_patterns: [] },
    linear: {
      token_env_var: "LINEAR_TOKEN",
      team_key: "ENG",
      my_email: "jan@test.com",
      actions: [{ name: "Analyze", icon: "search", prompt: "Analyze {{issue.title}}" }],
    },
  },
};

const mockIssues = [
  {
    id: "issue-1",
    identifier: "ENG-142",
    title: "Fix auth token refresh",
    description: "The auth token refresh is broken",
    priority: 2,
    priorityLabel: "High",
    state: { id: "s1", name: "In Progress", type: "started", color: "#f2c94c" },
    assignee: { id: "u1", name: "Jan", email: "jan@test.com", displayName: "Jan N" },
    labels: { nodes: [{ id: "l1", name: "bug", color: "#eb5757" }] },
    cycle: null,
    url: "https://linear.app/eng/issue/ENG-142",
    createdAt: "2026-03-15T10:00:00Z",
    updatedAt: "2026-03-16T10:00:00Z",
  },
];

const mockProject = {
  id: "proj-1",
  host_id: "host-1",
  path: "/home/user/project",
  name: "myproject",
  has_claude_config: false,
  project_type: "rust",
  created_at: "2026-03-01",
  parent_project_id: null,
  git_branch: "main",
  git_commit_hash: "abc1234",
  git_commit_message: "test",
  git_is_dirty: false,
  git_ahead: 0,
  git_behind: 0,
  git_remotes: null,
  git_updated_at: null,
  has_zremote_config: true,
};

beforeEach(() => {
  vi.restoreAllMocks();
});

describe("LinearIssuesPanel", () => {
  test("shows not-configured state when no linear settings", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/settings")) {
        return mockFetchResponse({
          settings: {
            env: {},
            agentic: { auto_detect: true, default_permissions: [], auto_approve_patterns: [] },
          },
        });
      }
      if (url.includes("/projects/proj-1") && !url.includes("/linear")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    render(
      <MemoryRouter>
        <LinearIssuesPanel projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(
        screen.getByText("Linear integration is not configured for this project."),
      ).toBeInTheDocument();
    });
  });

  test("renders skeleton loading state", () => {
    global.fetch = vi.fn().mockReturnValue(new Promise(() => {}));

    render(
      <MemoryRouter>
        <LinearIssuesPanel projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    const skeletons = document.querySelectorAll(".animate-pulse");
    expect(skeletons.length).toBeGreaterThan(0);
  });

  test("renders issue list with correct data", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/settings")) {
        return mockFetchResponse(mockSettings);
      }
      if (url.includes("/linear/issues")) {
        return mockFetchResponse(mockIssues);
      }
      if (url.includes("/projects/proj-1") && !url.includes("/linear")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    render(
      <MemoryRouter>
        <LinearIssuesPanel projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("ENG-142")).toBeInTheDocument();
      expect(screen.getByText("Fix auth token refresh")).toBeInTheDocument();
    });
  });

  test("shows empty state when no issues match", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/settings")) {
        return mockFetchResponse(mockSettings);
      }
      if (url.includes("/linear/issues")) {
        return mockFetchResponse([]);
      }
      if (url.includes("/projects/proj-1") && !url.includes("/linear")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    render(
      <MemoryRouter>
        <LinearIssuesPanel projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("No issues match your filters")).toBeInTheDocument();
    });
  });

  test("shows error state with retry button", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/settings")) {
        return mockFetchResponse(mockSettings);
      }
      if (url.includes("/linear/issues")) {
        return Promise.resolve({
          ok: false,
          text: async () => "Linear API error",
          json: async () => ({}),
        });
      }
      if (url.includes("/projects/proj-1") && !url.includes("/linear")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    render(
      <MemoryRouter>
        <LinearIssuesPanel projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("Failed to load Linear issues")).toBeInTheDocument();
      expect(screen.getByText("Retry")).toBeInTheDocument();
    });
  });

  test("filter preset changes trigger re-fetch", async () => {
    const fetchMock = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/settings")) {
        return mockFetchResponse(mockSettings);
      }
      if (url.includes("/linear/issues")) {
        return mockFetchResponse(mockIssues);
      }
      if (url.includes("/projects/proj-1") && !url.includes("/linear")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });
    global.fetch = fetchMock;

    render(
      <MemoryRouter>
        <LinearIssuesPanel projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("ENG-142")).toBeInTheDocument();
    });

    // Click on Backlog filter
    await userEvent.click(screen.getByText("Backlog"));

    await waitFor(() => {
      // Should have made another fetch call with preset=backlog
      const issueCalls = fetchMock.mock.calls.filter(
        (c: unknown[]) => typeof c[0] === "string" && (c[0] as string).includes("/linear/issues"),
      );
      expect(issueCalls.length).toBeGreaterThanOrEqual(2);
    });
  });

  test("clicking issue shows detail panel", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/settings")) {
        return mockFetchResponse(mockSettings);
      }
      if (url.includes("/linear/issues")) {
        return mockFetchResponse(mockIssues);
      }
      if (url.includes("/projects/proj-1") && !url.includes("/linear")) {
        return mockFetchResponse(mockProject);
      }
      return mockFetchResponse({});
    });

    render(
      <MemoryRouter>
        <LinearIssuesPanel projectId="proj-1" hostId="host-1" />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("ENG-142")).toBeInTheDocument();
    });

    // Click on the issue
    await userEvent.click(screen.getByText("Fix auth token refresh"));

    await waitFor(() => {
      // Detail panel should show description and action buttons
      expect(screen.getByText("The auth token refresh is broken")).toBeInTheDocument();
      expect(screen.getByText("Analyze")).toBeInTheDocument();
    });
  });
});
