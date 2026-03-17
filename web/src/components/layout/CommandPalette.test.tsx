import { render, screen, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { CommandPalette } from "./CommandPalette";
import type { Host, Project } from "../../lib/api";

// cmdk uses ResizeObserver and scrollIntoView
vi.stubGlobal(
  "ResizeObserver",
  class {
    observe() {}
    unobserve() {}
    disconnect() {}
  },
);

// jsdom doesn't implement scrollIntoView
Element.prototype.scrollIntoView = vi.fn();

let mockHosts: Host[] = [];
let mockIsLocal = false;
let mockProjects: Project[] = [];

// Mock hooks that fetch data
vi.mock("../../hooks/useHosts", () => ({
  useHosts: () => ({ hosts: mockHosts, loading: false, error: null }),
}));

vi.mock("../../hooks/useMode", () => ({
  useMode: () => ({ mode: mockIsLocal ? "local" : "server", isLocal: mockIsLocal }),
}));

vi.mock("../../hooks/useProjects", () => ({
  useProjects: () => ({ projects: mockProjects, loading: false }),
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

const mockProject: Project = {
  id: "proj-1",
  host_id: "host-1",
  path: "/home/user/project",
  name: "my-project",
  has_claude_config: false,
  has_zremote_config: false,
  project_type: "node",
  created_at: new Date().toISOString(),
  parent_project_id: null,
  git_branch: "main",
  git_commit_hash: "abc123",
  git_commit_message: "init",
  git_is_dirty: false,
  git_ahead: 0,
  git_behind: 0,
  git_remotes: null,
  git_updated_at: null,
};

function openPalette() {
  act(() => {
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "k", ctrlKey: true }),
    );
  });
}

describe("CommandPalette", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    mockHosts = [];
    mockIsLocal = false;
    mockProjects = [];
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => [],
      text: async () => "[]",
    });
  });

  test("renders nothing when closed", () => {
    const { container } = render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );
    expect(container.children.length).toBe(0);
  });

  test("opens on Ctrl+K", () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    expect(screen.getByPlaceholderText("Search commands...")).toBeInTheDocument();
  });

  test("opens on Meta+K (Mac)", () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    act(() => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "k", metaKey: true }),
      );
    });

    expect(screen.getByPlaceholderText("Search commands...")).toBeInTheDocument();
  });

  test("shows navigation commands when open", () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    expect(screen.getByText("Open Analytics")).toBeInTheDocument();
    expect(screen.getByText("Open History")).toBeInTheDocument();
    expect(screen.getByText("Open Settings")).toBeInTheDocument();
  });

  test("closes when backdrop is clicked", async () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();
    expect(screen.getByPlaceholderText("Search commands...")).toBeInTheDocument();

    // Click backdrop (the div with bg-black/50)
    const backdrop = document.querySelector(".bg-black\\/50");
    await userEvent.click(backdrop!);

    expect(screen.queryByPlaceholderText("Search commands...")).not.toBeInTheDocument();
  });

  test("toggles off with second Ctrl+K press", () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();
    expect(screen.getByPlaceholderText("Search commands...")).toBeInTheDocument();

    openPalette();
    expect(screen.queryByPlaceholderText("Search commands...")).not.toBeInTheDocument();
  });

  test("shows hosts group in server mode with hosts", () => {
    mockHosts = [mockHost];

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    expect(screen.getByText("Go to my-server")).toBeInTheDocument();
  });

  test("does not show hosts group in local mode", () => {
    mockHosts = [mockHost];
    mockIsLocal = true;

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    expect(screen.queryByText("Go to my-server")).not.toBeInTheDocument();
  });

  test("shows Start Claude group when projects and online host exist", () => {
    mockHosts = [mockHost];
    mockProjects = [mockProject];

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    expect(screen.getByText("Start Claude on my-project")).toBeInTheDocument();
  });

  test("does not show Start Claude group when no projects", () => {
    mockHosts = [mockHost];
    mockProjects = [];

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    expect(screen.queryByText(/Start Claude on/)).not.toBeInTheDocument();
  });

  test("shows No results found for non-matching search", async () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    const input = screen.getByPlaceholderText("Search commands...");
    await userEvent.type(input, "xyznonexistent");

    expect(screen.getByText("No results found")).toBeInTheDocument();
  });

  test("shows keyboard hints at bottom", () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    expect(screen.getByText("Navigate with arrow keys")).toBeInTheDocument();
    expect(screen.getByText("Esc")).toBeInTheDocument();
    expect(screen.getByText("to close")).toBeInTheDocument();
  });

  test("clicking Start Claude option opens StartClaudeDialog", async () => {
    mockHosts = [mockHost];
    mockProjects = [mockProject];

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    const startClaudeItem = screen.getByText("Start Claude on my-project");
    await userEvent.click(startClaudeItem);

    // Command palette closes and StartClaudeDialog opens
    expect(screen.queryByPlaceholderText("Search commands...")).not.toBeInTheDocument();
    expect(screen.getByText("Project: my-project")).toBeInTheDocument();
  });
});
