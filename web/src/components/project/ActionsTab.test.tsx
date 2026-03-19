import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { ActionsTab } from "./ActionsTab";

function mockFetchResponse(data: unknown) {
  return Promise.resolve({
    ok: true,
    text: async () => JSON.stringify(data),
    json: async () => data,
  });
}

beforeEach(() => {
  vi.restoreAllMocks();
});

function renderTab() {
  return render(
    <MemoryRouter>
      <ActionsTab
        projectId="proj-1"
        projectPath="/home/user/project"
        hostId="host-1"
      />
    </MemoryRouter>,
  );
}

describe("ActionsTab", () => {
  test("renders empty state when no actions", async () => {
    global.fetch = vi.fn().mockImplementation(() =>
      mockFetchResponse({ actions: [] }),
    );
    renderTab();
    await waitFor(() => {
      expect(screen.getByText("No actions configured")).toBeInTheDocument();
    });
  });

  test("renders action cards when actions exist", async () => {
    global.fetch = vi.fn().mockImplementation(() =>
      mockFetchResponse({
        actions: [
          {
            name: "build",
            command: "cargo build",
            description: "Build project",
            env: {},
            worktree_scoped: false,
          },
          {
            name: "test",
            command: "cargo test",
            description: "Run tests",
            env: {},
            worktree_scoped: false,
          },
        ],
      }),
    );
    renderTab();
    await waitFor(() => {
      expect(screen.getByText("build")).toBeInTheDocument();
      expect(screen.getByText("test")).toBeInTheDocument();
    });
  });

  test("separates project and worktree-scoped actions", async () => {
    global.fetch = vi.fn().mockImplementation(() =>
      mockFetchResponse({
        actions: [
          {
            name: "deploy",
            command: "make deploy",
            env: {},
            worktree_scoped: false,
          },
          {
            name: "setup",
            command: "bun install",
            env: {},
            worktree_scoped: true,
          },
        ],
      }),
    );
    renderTab();
    await waitFor(() => {
      expect(screen.getByText("Project Actions")).toBeInTheDocument();
      expect(screen.getByText("Worktree Actions")).toBeInTheDocument();
      expect(screen.getByText("deploy")).toBeInTheDocument();
      expect(screen.getByText("setup")).toBeInTheDocument();
    });
  });

  test("renders loading skeleton", () => {
    global.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    renderTab();
    const skeletons = document.querySelectorAll(".animate-pulse");
    expect(skeletons.length).toBeGreaterThan(0);
  });
});
