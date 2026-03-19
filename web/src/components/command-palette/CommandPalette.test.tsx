import { render, screen, act, waitFor, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { CommandPalette } from "./CommandPalette";
import type { Host, Project, Session } from "../../lib/api";
import { useCommandPaletteStore } from "../../stores/command-palette-store";

// cmdk uses ResizeObserver and scrollIntoView
vi.stubGlobal(
  "ResizeObserver",
  class {
    observe() {}
    unobserve() {}
    disconnect() {}
  },
);

Element.prototype.scrollIntoView = vi.fn();

// Radix Dialog (used by Command.Dialog) needs PointerEvent and pointer capture in JSDOM
class MockPointerEvent extends MouseEvent {
  readonly pointerId: number;
  readonly pointerType: string;
  constructor(type: string, props: PointerEventInit & { pointerId?: number; pointerType?: string } = {}) {
    super(type, props);
    this.pointerId = props.pointerId ?? 0;
    this.pointerType = props.pointerType ?? "";
  }
}
vi.stubGlobal("PointerEvent", MockPointerEvent);

HTMLElement.prototype.hasPointerCapture = vi.fn().mockReturnValue(false);
HTMLElement.prototype.setPointerCapture = vi.fn();
HTMLElement.prototype.releasePointerCapture = vi.fn();

let mockHosts: Host[] = [];
let mockIsLocal = false;

vi.mock("../../hooks/useHosts", () => ({
  useHosts: () => ({ hosts: mockHosts, loading: false, error: null }),
}));

vi.mock("../../hooks/useMode", () => ({
  useMode: () => ({
    mode: mockIsLocal ? "local" : "server",
    isLocal: mockIsLocal,
  }),
}));

// Mock useCommandPaletteContext to return global by default
let mockRouteContext = { level: "global" as const };
vi.mock("../../hooks/useCommandPaletteContext", () => ({
  useCommandPaletteContext: () => mockRouteContext,
}));

// Mock api calls
vi.mock("../../lib/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../lib/api")>();
  return {
    ...original,
    api: {
      hosts: { list: vi.fn().mockResolvedValue([]), get: vi.fn() },
      sessions: {
        list: vi.fn().mockResolvedValue([]),
        get: vi.fn(),
        create: vi.fn().mockResolvedValue({ id: "new-session", host_id: "host-1", status: "active" }),
        close: vi.fn(),
        rename: vi.fn(),
      },
      projects: {
        list: vi.fn().mockResolvedValue([]),
        get: vi.fn(),
        scan: vi.fn(),
        delete: vi.fn(),
        sessions: vi.fn().mockResolvedValue([]),
        worktrees: vi.fn().mockResolvedValue([]),
        actions: vi.fn().mockResolvedValue({ actions: [] }),
        refreshGit: vi.fn(),
        configureWithClaude: vi.fn(),
      },
      loops: {
        list: vi.fn().mockResolvedValue([]),
        get: vi.fn(),
        action: vi.fn(),
      },
      claudeTasks: {
        list: vi.fn().mockResolvedValue([]),
        create: vi.fn(),
        resume: vi.fn(),
      },
      knowledge: {
        triggerIndex: vi.fn(),
      },
      search: { transcripts: vi.fn() },
    },
  };
});

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

const mockSession: Session = {
  id: "session-1",
  host_id: "host-1",
  name: "dev terminal",
  shell: "/bin/bash",
  status: "active",
  cols: 80,
  rows: 24,
  created_at: new Date().toISOString(),
  closed_at: null,
  exit_code: null,
  working_dir: "/home/user",
  project_id: null,
  tmux_name: null,
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
    mockRouteContext = { level: "global" as const };
    // Reset store
    act(() => {
      useCommandPaletteStore.setState({
        open: false,
        contextStack: [{ level: "global" }],
        query: "",
      });
    });
  });

  test("opens on Ctrl+K", () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    expect(
      screen.getByPlaceholderText("Search commands..."),
    ).toBeInTheDocument();
  });

  test("opens on Double-Shift", () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    // First shift
    act(() => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Shift", bubbles: true }),
      );
    });

    // Second shift quickly
    act(() => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Shift", bubbles: true }),
      );
    });

    expect(
      screen.getByPlaceholderText("Search commands..."),
    ).toBeInTheDocument();
  });

  test("shows navigation actions at global level", () => {
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

  test("shows host items in server mode", async () => {
    mockHosts = [mockHost];

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });
  });

  test("does not show host drill-down items in local mode", async () => {
    mockHosts = [mockHost];
    mockIsLocal = true;

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // In local mode, the palette auto-escalates to host level,
    // so we should NOT see the host name as a drill-down item
    // We should see host-level actions instead
    await waitFor(() => {
      // Host actions like "New terminal session" should appear
      expect(screen.getByText("New terminal session")).toBeInTheDocument();
    });

    // The host name should not appear as a navigable drill-down item
    expect(screen.queryByText("my-server")).not.toBeInTheDocument();
  });

  test("drill-down into host shows host actions", async () => {
    mockHosts = [mockHost];

    const { api } = await import("../../lib/api");
    vi.mocked(api.projects.list).mockResolvedValue([mockProject]);
    vi.mocked(api.sessions.list).mockResolvedValue([mockSession]);

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // Click host to drill down
    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("my-server"));

    // Now we should see host-level actions
    await waitFor(() => {
      expect(screen.getByText("New terminal session")).toBeInTheDocument();
    });
  });

  test("backspace on empty input goes up", async () => {
    mockHosts = [mockHost];

    const { api } = await import("../../lib/api");
    vi.mocked(api.projects.list).mockResolvedValue([]);
    vi.mocked(api.sessions.list).mockResolvedValue([]);

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // Drill into host
    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });
    await userEvent.click(screen.getByText("my-server"));

    // Wait for host level to load
    await waitFor(() => {
      expect(screen.getByText("New terminal session")).toBeInTheDocument();
    });

    // Press backspace on the empty input to go back
    const input = screen.getByPlaceholderText("Search commands...");
    await userEvent.click(input);
    await userEvent.keyboard("{Backspace}");

    // Should be back at global level showing host as drill-down
    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });
  });

  test("breadcrumb display updates on drill-down", async () => {
    mockHosts = [mockHost];

    const { api } = await import("../../lib/api");
    vi.mocked(api.projects.list).mockResolvedValue([]);
    vi.mocked(api.sessions.list).mockResolvedValue([]);

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // At global level, no breadcrumb pills (only 1 item in stack)
    expect(screen.queryByRole("button", { name: "Global" })).not.toBeInTheDocument();

    // Drill into host
    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });
    await userEvent.click(screen.getByText("my-server"));

    // Should now see breadcrumb pill for "Global"
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Global" })).toBeInTheDocument();
    });
  });

  test("closes on Escape", async () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();
    expect(
      screen.getByPlaceholderText("Search commands..."),
    ).toBeInTheDocument();

    await userEvent.keyboard("{Escape}");

    await waitFor(() => {
      expect(
        screen.queryByPlaceholderText("Search commands..."),
      ).not.toBeInTheDocument();
    });
  });

  test("closes when backdrop is clicked", async () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();
    expect(
      screen.getByPlaceholderText("Search commands..."),
    ).toBeInTheDocument();

    // Radix Dialog renders overlay with cmdk-overlay attribute
    const backdrop = document.querySelector("[cmdk-overlay]") ?? document.querySelector(".bg-black\\/50");
    await userEvent.click(backdrop!);

    await waitFor(() => {
      expect(
        screen.queryByPlaceholderText("Search commands..."),
      ).not.toBeInTheDocument();
    });
  });

  test("dialog spawning works (StartClaude)", async () => {
    mockHosts = [mockHost];
    mockIsLocal = true;

    const { api } = await import("../../lib/api");
    vi.mocked(api.projects.list).mockResolvedValue([mockProject]);
    vi.mocked(api.sessions.list).mockResolvedValue([]);
    vi.mocked(api.projects.get).mockResolvedValue(mockProject);
    vi.mocked(api.projects.sessions).mockResolvedValue([]);
    vi.mocked(api.projects.worktrees).mockResolvedValue([]);
    vi.mocked(api.projects.actions).mockResolvedValue({ actions: [] });
    vi.mocked(api.claudeTasks.list).mockResolvedValue([]);

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // In local mode, palette auto-escalates to host level, showing projects as drill-down
    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });

    // Drill into project
    await userEvent.click(screen.getByText("my-project"));

    // Now at project level, click "Start Claude"
    await waitFor(() => {
      expect(screen.getByText("Start Claude")).toBeInTheDocument();
    });
    await userEvent.click(screen.getByText("Start Claude"));

    // Palette closes, StartClaudeDialog opens
    expect(
      screen.queryByPlaceholderText("Search commands..."),
    ).not.toBeInTheDocument();
    expect(screen.getByText("Project: my-project")).toBeInTheDocument();
  });

  test("search filtering works", async () => {
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

  test("shows keyboard hints in footer", () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // Footer contains Esc and Close
    expect(screen.getByText("Esc")).toBeInTheDocument();
    expect(screen.getByText("Close")).toBeInTheDocument();
    expect(screen.getByText("Select")).toBeInTheDocument();
    // "Navigate" appears both as a group heading and footer text, just check it exists
    expect(screen.getAllByText("Navigate").length).toBeGreaterThanOrEqual(1);
  });

  test("shows Back hint when drilled down", async () => {
    mockHosts = [mockHost];

    const { api } = await import("../../lib/api");
    vi.mocked(api.projects.list).mockResolvedValue([]);
    vi.mocked(api.sessions.list).mockResolvedValue([]);

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // Not drilled down yet - no Back hint
    expect(screen.queryByText("Back")).not.toBeInTheDocument();

    // Drill into host
    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });
    await userEvent.click(screen.getByText("my-server"));

    // Should show Back hint
    await waitFor(() => {
      expect(screen.getByText("Back")).toBeInTheDocument();
    });
  });

  test("shows context level indicator in footer", () => {
    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // At global level, footer should show "Global"
    expect(screen.getByText("Global")).toBeInTheDocument();
  });

  test("shows context level indicator as Host when drilled into host", async () => {
    mockHosts = [mockHost];

    const { api } = await import("../../lib/api");
    vi.mocked(api.projects.list).mockResolvedValue([]);
    vi.mocked(api.sessions.list).mockResolvedValue([]);

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // Drill into host
    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });
    await userEvent.click(screen.getByText("my-server"));

    // Footer should show "Host"
    await waitFor(() => {
      expect(screen.getByText("Host")).toBeInTheDocument();
    });
  });

  test("custom project actions are displayed", async () => {
    mockHosts = [mockHost];
    mockIsLocal = true;

    const customAction = {
      name: "deploy",
      command: "make deploy",
      description: "Deploy to production",
      env: {},
      worktree_scoped: false,
    };

    const { api } = await import("../../lib/api");
    vi.mocked(api.projects.list).mockResolvedValue([mockProject]);
    vi.mocked(api.sessions.list).mockResolvedValue([]);
    vi.mocked(api.projects.get).mockResolvedValue(mockProject);
    vi.mocked(api.projects.sessions).mockResolvedValue([]);
    vi.mocked(api.projects.worktrees).mockResolvedValue([]);
    vi.mocked(api.projects.actions).mockResolvedValue({ actions: [customAction] });
    vi.mocked(api.claudeTasks.list).mockResolvedValue([]);

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // In local mode, drill into project
    await waitFor(() => {
      expect(screen.getByText("my-project")).toBeInTheDocument();
    });
    await userEvent.click(screen.getByText("my-project"));

    // Custom action should appear
    await waitFor(() => {
      expect(screen.getByText("Deploy to production")).toBeInTheDocument();
    });
  });

  test("breadcrumb click navigates to that level", async () => {
    mockHosts = [mockHost];

    const { api } = await import("../../lib/api");
    vi.mocked(api.projects.list).mockResolvedValue([mockProject]);
    vi.mocked(api.sessions.list).mockResolvedValue([]);

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // Drill into host
    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });
    await userEvent.click(screen.getByText("my-server"));

    // Should see breadcrumb with "Global" pill
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Global" })).toBeInTheDocument();
    });

    // Click "Global" breadcrumb to go back
    await userEvent.click(screen.getByRole("button", { name: "Global" }));

    // Should be back at global level with host drill-down visible
    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });

    // Breadcrumb should be gone (only 1 item in stack)
    expect(screen.queryByRole("button", { name: "Global" })).not.toBeInTheDocument();
  });

  test("Tab drills into item with drillDown", async () => {
    mockHosts = [mockHost];

    const { api } = await import("../../lib/api");
    vi.mocked(api.projects.list).mockResolvedValue([]);
    vi.mocked(api.sessions.list).mockResolvedValue([]);

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // Wait for host item to appear (it has a drillDown)
    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });

    // Navigate down to "my-server" (Item 7: 7 ArrowDown from first selected item, +1 for Clipboard History, +1 for Switch Session)
    await userEvent.keyboard("{ArrowDown}");
    await userEvent.keyboard("{ArrowDown}");
    await userEvent.keyboard("{ArrowDown}");
    await userEvent.keyboard("{ArrowDown}");
    await userEvent.keyboard("{ArrowDown}");
    await userEvent.keyboard("{ArrowDown}");
    await userEvent.keyboard("{ArrowDown}");

    // Verify my-server is now selected
    const hostItem = screen.getByText("my-server").closest("[cmdk-item]")!;
    expect(hostItem.getAttribute("data-selected")).toBe("true");

    // Press Tab to drill into the highlighted item (fire on cmdk-root for React event delegation)
    const cmdkRoot = document.querySelector("[cmdk-root]")!;
    fireEvent.keyDown(cmdkRoot, { key: "Tab" });

    // Should now be at host level
    await waitFor(() => {
      expect(screen.getByText("New terminal session")).toBeInTheDocument();
    });
  });

  test("Right arrow on empty input drills into drillDown item", async () => {
    mockHosts = [mockHost];

    const { api } = await import("../../lib/api");
    vi.mocked(api.projects.list).mockResolvedValue([]);
    vi.mocked(api.sessions.list).mockResolvedValue([]);

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });

    // Navigate down to "my-server" (Item 7, +1 for Clipboard History, +1 for Switch Session)
    await userEvent.keyboard("{ArrowDown}");
    await userEvent.keyboard("{ArrowDown}");
    await userEvent.keyboard("{ArrowDown}");
    await userEvent.keyboard("{ArrowDown}");
    await userEvent.keyboard("{ArrowDown}");
    await userEvent.keyboard("{ArrowDown}");
    await userEvent.keyboard("{ArrowDown}");

    const hostItem = screen.getByText("my-server").closest("[cmdk-item]")!;
    expect(hostItem.getAttribute("data-selected")).toBe("true");

    // Right arrow on empty input should drill down (fire on cmdk-root)
    const cmdkRoot = document.querySelector("[cmdk-root]")!;
    fireEvent.keyDown(cmdkRoot, { key: "ArrowRight" });

    await waitFor(() => {
      expect(screen.getByText("New terminal session")).toBeInTheDocument();
    });
  });

  test("Left arrow on empty input pops context", async () => {
    mockHosts = [mockHost];

    const { api } = await import("../../lib/api");
    vi.mocked(api.projects.list).mockResolvedValue([]);
    vi.mocked(api.sessions.list).mockResolvedValue([]);

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // Drill into host first
    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });
    await userEvent.click(screen.getByText("my-server"));

    await waitFor(() => {
      expect(screen.getByText("New terminal session")).toBeInTheDocument();
    });

    // Left arrow on empty input should go back
    await userEvent.keyboard("{ArrowLeft}");

    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });
  });

  test("Shift+Tab pops context", async () => {
    mockHosts = [mockHost];

    const { api } = await import("../../lib/api");
    vi.mocked(api.projects.list).mockResolvedValue([]);
    vi.mocked(api.sessions.list).mockResolvedValue([]);

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // Drill into host first
    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });
    await userEvent.click(screen.getByText("my-server"));

    await waitFor(() => {
      expect(screen.getByText("New terminal session")).toBeInTheDocument();
    });

    // Shift+Tab should go back
    await userEvent.keyboard("{Shift>}{Tab}{/Shift}");

    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });
  });

  test("shows Drill down hint when drillDown items exist", async () => {
    mockHosts = [mockHost];

    render(
      <MemoryRouter>
        <CommandPalette />
      </MemoryRouter>,
    );

    openPalette();

    // Host items have drillDown, so "Drill down" hint should appear
    await waitFor(() => {
      expect(screen.getByText("my-server")).toBeInTheDocument();
    });
    expect(screen.getByText("Drill down")).toBeInTheDocument();
    expect(screen.getByText("Tab")).toBeInTheDocument();
  });
});
