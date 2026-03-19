import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { ProjectSettingsTab } from "./ProjectSettingsTab";

function mockFetchResponse(data: unknown) {
  return Promise.resolve({
    ok: true,
    text: async () => JSON.stringify(data),
    json: async () => data,
  });
}

const defaultSettings = {
  env: {},
  agentic: {
    auto_detect: true,
    default_permissions: [],
    auto_approve_patterns: [],
  },
};

const fullSettings = {
  shell: "/bin/zsh",
  working_dir: "/home/user/project/src",
  env: { RUST_LOG: "debug", NODE_ENV: "development" },
  agentic: {
    auto_detect: false,
    default_permissions: ["Read", "Glob"],
    auto_approve_patterns: ["cargo test*"],
  },
};

beforeEach(() => {
  vi.restoreAllMocks();
});

describe("ProjectSettingsTab", () => {
  test("renders loading state", () => {
    global.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    render(
      <MemoryRouter>
        <ProjectSettingsTab
          projectId="proj-1"
          projectPath="/home/user/project"
          hostId="host-1"
        />
      </MemoryRouter>,
    );
    expect(screen.getByText("Loading settings...")).toBeInTheDocument();
  });

  test("shows no settings state when settings is null", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      text: async () => JSON.stringify({ settings: null }),
      json: async () => ({ settings: null }),
    });
    render(
      <MemoryRouter>
        <ProjectSettingsTab
          projectId="proj-1"
          projectPath="/home/user/project"
          hostId="host-1"
        />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(screen.getByText("Create Settings")).toBeInTheDocument();
    });
  });

  test("displays settings values after load", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/settings")) {
        return mockFetchResponse({ settings: fullSettings });
      }
      return mockFetchResponse({});
    });
    render(
      <MemoryRouter>
        <ProjectSettingsTab
          projectId="proj-1"
          projectPath="/home/user/project"
          hostId="host-1"
        />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(screen.getByDisplayValue("/bin/zsh")).toBeInTheDocument();
      expect(
        screen.getByDisplayValue("/home/user/project/src"),
      ).toBeInTheDocument();
      expect(screen.getByDisplayValue("RUST_LOG")).toBeInTheDocument();
      expect(screen.getByDisplayValue("debug")).toBeInTheDocument();
      expect(screen.getByDisplayValue("NODE_ENV")).toBeInTheDocument();
      expect(screen.getByDisplayValue("development")).toBeInTheDocument();
      expect(
        screen.getByDisplayValue("Read, Glob"),
      ).toBeInTheDocument();
      expect(
        screen.getByDisplayValue("cargo test*"),
      ).toBeInTheDocument();
    });
  });

  test("env var name validation shows inline error on invalid name", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/settings")) {
        return mockFetchResponse({ settings: defaultSettings });
      }
      return mockFetchResponse({});
    });
    render(
      <MemoryRouter>
        <ProjectSettingsTab
          projectId="proj-1"
          projectPath="/home/user/project"
          hostId="host-1"
        />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(screen.getByText("General")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("Add variable"));

    const nameInputs = screen.getAllByLabelText("Variable name");
    await userEvent.type(nameInputs[0], "123-BAD");

    await waitFor(() => {
      expect(
        screen.getByText("Invalid name: use letters, digits, underscore"),
      ).toBeInTheDocument();
    });
  });

  test("save button calls API", async () => {
    const fetchMock = vi.fn().mockImplementation((url: string, opts?: RequestInit) => {
      if (opts?.method === "PUT" && url.includes("/settings")) {
        return Promise.resolve({
          ok: true,
          text: async () => "",
          json: async () => undefined,
        });
      }
      if (url.includes("/settings")) {
        return mockFetchResponse({ settings: defaultSettings });
      }
      return mockFetchResponse({});
    });
    global.fetch = fetchMock;

    render(
      <MemoryRouter>
        <ProjectSettingsTab
          projectId="proj-1"
          projectPath="/home/user/project"
          hostId="host-1"
        />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(screen.getByText("General")).toBeInTheDocument();
    });

    // Make a change to enable save
    const shellInput = screen.getByPlaceholderText("System default");
    await userEvent.type(shellInput, "/bin/bash");

    await waitFor(() => {
      expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("Save Settings"));

    await waitFor(() => {
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/projects/proj-1/settings",
        expect.objectContaining({ method: "PUT" }),
      );
    });
  });

  test("create settings button saves defaults", async () => {
    const fetchMock = vi.fn().mockImplementation((url: string, opts?: RequestInit) => {
      if (opts?.method === "PUT" && url.includes("/settings")) {
        return Promise.resolve({
          ok: true,
          text: async () => "",
          json: async () => undefined,
        });
      }
      if (url.includes("/settings")) {
        return Promise.resolve({
          ok: true,
          text: async () => JSON.stringify({ settings: null }),
          json: async () => ({ settings: null }),
        });
      }
      return mockFetchResponse({});
    });
    global.fetch = fetchMock;

    render(
      <MemoryRouter>
        <ProjectSettingsTab
          projectId="proj-1"
          projectPath="/home/user/project"
          hostId="host-1"
        />
      </MemoryRouter>,
    );

    await waitFor(() => {
      expect(screen.getByText("Create Settings")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("Create Settings"));

    await waitFor(() => {
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/projects/proj-1/settings",
        expect.objectContaining({ method: "PUT" }),
      );
    });
  });

  test("renders configure with claude button in empty state", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      text: async () => JSON.stringify({ settings: null }),
      json: async () => ({ settings: null }),
    });
    render(
      <MemoryRouter>
        <ProjectSettingsTab
          projectId="proj-1"
          projectPath="/home/user/project"
          hostId="host-1"
        />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(screen.getByText("Create Settings")).toBeInTheDocument();
      expect(screen.getByText("Configure with Claude")).toBeInTheDocument();
    });
  });

  test("shows error state with reset button", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: false,
      text: async () => "Malformed JSON in settings file",
      json: async () => ({}),
    });
    render(
      <MemoryRouter>
        <ProjectSettingsTab
          projectId="proj-1"
          projectPath="/home/user/project"
          hostId="host-1"
        />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(screen.getByText("Failed to load settings")).toBeInTheDocument();
      expect(screen.getByText("Reset to defaults")).toBeInTheDocument();
    });
  });

  test("renders linear section when settings have linear config", async () => {
    const settingsWithLinear = {
      ...defaultSettings,
      linear: {
        token_env_var: "MY_TOKEN",
        team_key: "ENG",
        my_email: "jan@test.com",
        actions: [{ name: "Analyze", prompt: "Analyze {{issue.title}}" }],
      },
    };
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/settings")) {
        return mockFetchResponse({ settings: settingsWithLinear });
      }
      return mockFetchResponse({});
    });
    render(
      <MemoryRouter>
        <ProjectSettingsTab projectId="proj-1" projectPath="/home/user/project" hostId="host-1" />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(screen.getByText("Linear Integration")).toBeInTheDocument();
      expect(screen.getByDisplayValue("MY_TOKEN")).toBeInTheDocument();
      expect(screen.getByDisplayValue("ENG")).toBeInTheDocument();
      expect(screen.getByDisplayValue("jan@test.com")).toBeInTheDocument();
    });
  });

  test("linear toggle shows fields when enabled", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/settings")) {
        return mockFetchResponse({ settings: defaultSettings });
      }
      return mockFetchResponse({});
    });
    render(
      <MemoryRouter>
        <ProjectSettingsTab projectId="proj-1" projectPath="/home/user/project" hostId="host-1" />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(screen.getByText("Linear Integration")).toBeInTheDocument();
    });

    // Linear fields should not be visible initially
    expect(screen.queryByPlaceholderText("e.g. ENG")).not.toBeInTheDocument();

    // Enable linear
    await userEvent.click(screen.getByLabelText("Enable Linear integration"));

    await waitFor(() => {
      expect(screen.getByText("Validate")).toBeInTheDocument();
      expect(screen.getByPlaceholderText("e.g. ENG")).toBeInTheDocument();
    });
  });

  test("linear validate calls API", async () => {
    const settingsWithLinear = {
      ...defaultSettings,
      linear: {
        token_env_var: "LINEAR_TOKEN",
        team_key: "ENG",
        actions: [],
      },
    };
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/linear/me")) {
        return mockFetchResponse({ id: "u1", name: "Jan", email: "jan@test.com", displayName: "Jan N" });
      }
      if (url.includes("/settings")) {
        return mockFetchResponse({ settings: settingsWithLinear });
      }
      return mockFetchResponse({});
    });
    render(
      <MemoryRouter>
        <ProjectSettingsTab projectId="proj-1" projectPath="/home/user/project" hostId="host-1" />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(screen.getByText("Validate")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("Validate"));

    await waitFor(() => {
      expect(screen.getByText(/Authenticated as Jan N/)).toBeInTheDocument();
    });
  });

  test("linear actions can be added and removed", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/settings")) {
        return mockFetchResponse({ settings: defaultSettings });
      }
      return mockFetchResponse({});
    });
    render(
      <MemoryRouter>
        <ProjectSettingsTab projectId="proj-1" projectPath="/home/user/project" hostId="host-1" />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(screen.getByText("Linear Integration")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByLabelText("Enable Linear integration"));

    await waitFor(() => {
      // Default actions should be added (3 starters)
      expect(screen.getAllByLabelText("Linear action name").length).toBe(3);
    });

    // Remove one action
    const removeButtons = screen.getAllByLabelText("Remove linear action");
    await userEvent.click(removeButtons[0]);

    await waitFor(() => {
      expect(screen.getAllByLabelText("Linear action name").length).toBe(2);
    });
  });
});
