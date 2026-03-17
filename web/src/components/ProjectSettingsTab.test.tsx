import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
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
      <ProjectSettingsTab
        projectId="proj-1"
        projectPath="/home/user/project"
        hostId="host-1"
      />,
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
      <ProjectSettingsTab
        projectId="proj-1"
        projectPath="/home/user/project"
        hostId="host-1"
      />,
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
      <ProjectSettingsTab
        projectId="proj-1"
        projectPath="/home/user/project"
        hostId="host-1"
      />,
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
      <ProjectSettingsTab
        projectId="proj-1"
        projectPath="/home/user/project"
        hostId="host-1"
      />,
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
      <ProjectSettingsTab
        projectId="proj-1"
        projectPath="/home/user/project"
        hostId="host-1"
      />,
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
      <ProjectSettingsTab
        projectId="proj-1"
        projectPath="/home/user/project"
        hostId="host-1"
      />,
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

  test("shows error state with reset button", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: false,
      text: async () => "Malformed JSON in settings file",
      json: async () => ({}),
    });
    render(
      <ProjectSettingsTab
        projectId="proj-1"
        projectPath="/home/user/project"
        hostId="host-1"
      />,
    );
    await waitFor(() => {
      expect(screen.getByText("Failed to load settings")).toBeInTheDocument();
      expect(screen.getByText("Reset to defaults")).toBeInTheDocument();
    });
  });
});
