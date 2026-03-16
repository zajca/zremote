import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { StartClaudeDialog } from "./StartClaudeDialog";
import type { ClaudeTask } from "../types/claude-session";

const mockCompletedTask: ClaudeTask = {
  id: "task-prev-1",
  session_id: "sess-prev-1",
  host_id: "host-1",
  project_path: "/path",
  project_id: "proj-1",
  model: "sonnet",
  initial_prompt: "Previous prompt",
  claude_session_id: null,
  resume_from: null,
  status: "completed",
  options_json: null,
  loop_id: null,
  started_at: new Date(Date.now() - 3600_000).toISOString(),
  ended_at: new Date().toISOString(),
  total_cost_usd: 1.23,
  total_tokens_in: 50_000,
  total_tokens_out: 20_000,
  summary: "Did some work",
  created_at: new Date().toISOString(),
};

beforeEach(() => {
  vi.restoreAllMocks();
  global.fetch = vi.fn().mockImplementation((url: string) => {
    if (url.includes("/api/claude-task")) {
      return Promise.resolve({
        ok: true,
        json: async () => [],
        text: async () => "[]",
      });
    }
    return Promise.resolve({
      ok: true,
      json: async () => ({}),
      text: async () => "{}",
    });
  });
});

function renderDialog(props: Partial<Parameters<typeof StartClaudeDialog>[0]> = {}) {
  return render(
    <MemoryRouter>
      <StartClaudeDialog
        projectName="test"
        projectPath="/path"
        hostId="host-1"
        onClose={vi.fn()}
        {...props}
      />
    </MemoryRouter>,
  );
}

describe("StartClaudeDialog", () => {
  test("renders dialog title", () => {
    renderDialog({ projectName: "my-project", projectPath: "/home/user/project" });
    expect(screen.getByRole("heading", { name: "Start Claude" })).toBeInTheDocument();
  });

  test("renders project name", () => {
    renderDialog({ projectName: "my-project" });
    expect(screen.getByText("Project: my-project")).toBeInTheDocument();
  });

  test("renders model selector buttons", () => {
    renderDialog();
    expect(screen.getByText("Sonnet")).toBeInTheDocument();
    expect(screen.getByText("Opus")).toBeInTheDocument();
    expect(screen.getByText("Haiku")).toBeInTheDocument();
  });

  test("renders prompt textarea", () => {
    renderDialog();
    expect(screen.getByPlaceholderText("What should Claude do?")).toBeInTheDocument();
  });

  test("renders Cancel and Start Claude buttons", () => {
    renderDialog();
    expect(screen.getByText("Cancel")).toBeInTheDocument();
    expect(screen.getAllByText("Start Claude").length).toBe(2);
  });

  test("calls onClose when Cancel is clicked", async () => {
    const onClose = vi.fn();
    renderDialog({ onClose });
    await userEvent.click(screen.getByText("Cancel"));
    expect(onClose).toHaveBeenCalled();
  });

  test("renders Options toggle", () => {
    renderDialog();
    expect(screen.getByText("Options")).toBeInTheDocument();
  });

  test("can select different model", async () => {
    renderDialog();
    const opusBtn = screen.getByText("Opus");
    await userEvent.click(opusBtn);
    // Opus button should now have the selected styling (bg-accent)
    expect(opusBtn.closest("button")).toHaveClass("bg-accent");
  });

  test("expands options section when Options is clicked", async () => {
    renderDialog();
    await userEvent.click(screen.getByText("Options"));
    expect(screen.getByText("Tool preset")).toBeInTheDocument();
    expect(screen.getByText("Skip permissions")).toBeInTheDocument();
    expect(screen.getByText("Custom flags")).toBeInTheDocument();
  });

  test("shows tool preset select with default value", async () => {
    renderDialog();
    await userEvent.click(screen.getByText("Options"));
    const select = screen.getByDisplayValue("Standard");
    expect(select).toBeInTheDocument();
  });

  test("shows custom tools input when custom preset selected", async () => {
    renderDialog();
    await userEvent.click(screen.getByText("Options"));
    const select = screen.getByDisplayValue("Standard");
    await userEvent.selectOptions(select, "custom");
    expect(screen.getByPlaceholderText("Read, Edit, Bash, Grep")).toBeInTheDocument();
  });

  test("shows skip permissions checkbox and warning", async () => {
    renderDialog();
    await userEvent.click(screen.getByText("Options"));
    const checkbox = screen.getByRole("checkbox");
    await userEvent.click(checkbox);
    expect(screen.getByText("Tools will run without approval")).toBeInTheDocument();
  });

  test("shows custom flags input in options", async () => {
    renderDialog();
    await userEvent.click(screen.getByText("Options"));
    expect(screen.getByPlaceholderText("--verbose --max-turns 50")).toBeInTheDocument();
  });

  test("shows error when submit fails", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/api/claude-task") && !url.includes("?")) {
        // POST to create task fails
        return Promise.resolve({
          ok: false,
          status: 500,
          text: async () => "Internal server error",
          statusText: "Internal Server Error",
        });
      }
      // GET list succeeds
      return Promise.resolve({
        ok: true,
        json: async () => [],
        text: async () => "[]",
      });
    });

    renderDialog();

    // Click Start Claude button (not heading)
    const buttons = screen.getAllByText("Start Claude");
    const startButton = buttons.find((el) => el.closest("button") && !el.closest("h2"));
    await userEvent.click(startButton!);

    await waitFor(() => {
      expect(screen.getByText("Internal server error")).toBeInTheDocument();
    });
  });

  test("shows completed tasks for resume when available", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/api/claude-task")) {
        return Promise.resolve({
          ok: true,
          json: async () => [mockCompletedTask],
          text: async () => JSON.stringify([mockCompletedTask]),
        });
      }
      return Promise.resolve({
        ok: true,
        json: async () => ({}),
        text: async () => "{}",
      });
    });

    renderDialog({ projectId: "proj-1" });

    await waitFor(() => {
      expect(screen.getByText("Resume previous")).toBeInTheDocument();
      expect(screen.getByText("Previous prompt")).toBeInTheDocument();
      expect(screen.getByText("Resume")).toBeInTheDocument();
    });
  });

  test("shows task summary in resume list when available", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/api/claude-task")) {
        return Promise.resolve({
          ok: true,
          json: async () => [mockCompletedTask],
          text: async () => JSON.stringify([mockCompletedTask]),
        });
      }
      return Promise.resolve({
        ok: true,
        json: async () => ({}),
        text: async () => "{}",
      });
    });

    renderDialog({ projectId: "proj-1" });

    await waitFor(() => {
      expect(screen.getByText("Did some work")).toBeInTheDocument();
    });
  });

  test("shows cost in resume task item", async () => {
    global.fetch = vi.fn().mockImplementation((url: string) => {
      if (url.includes("/api/claude-task")) {
        return Promise.resolve({
          ok: true,
          json: async () => [mockCompletedTask],
          text: async () => JSON.stringify([mockCompletedTask]),
        });
      }
      return Promise.resolve({
        ok: true,
        json: async () => ({}),
        text: async () => "{}",
      });
    });

    renderDialog({ projectId: "proj-1" });

    await waitFor(() => {
      expect(screen.getByText(/\$1\.23/)).toBeInTheDocument();
    });
  });

  test("calls onClose when backdrop is clicked", async () => {
    const onClose = vi.fn();
    renderDialog({ onClose });

    // The backdrop is the outermost div with the fixed inset-0 class
    const backdrop = screen.getByRole("heading", { name: "Start Claude" }).closest(".fixed");
    await userEvent.click(backdrop!);
    expect(onClose).toHaveBeenCalled();
  });

  test("shows Prompt label", () => {
    renderDialog();
    expect(screen.getByText("Prompt")).toBeInTheDocument();
  });

  test("shows Model label", () => {
    renderDialog();
    expect(screen.getByText("Model")).toBeInTheDocument();
  });

  test("allows typing in prompt textarea", async () => {
    renderDialog();
    const textarea = screen.getByPlaceholderText("What should Claude do?");
    await userEvent.type(textarea, "Write unit tests");
    expect(textarea).toHaveValue("Write unit tests");
  });

  test("shows Starting... state when submitting", async () => {
    // Make the create call hang
    global.fetch = vi.fn().mockImplementation((url: string, options?: RequestInit) => {
      if (url.includes("/api/claude-task") && options?.method === "POST") {
        return new Promise(() => {}); // never resolves
      }
      return Promise.resolve({
        ok: true,
        json: async () => [],
        text: async () => "[]",
      });
    });

    renderDialog();

    const buttons = screen.getAllByText("Start Claude");
    const startButton = buttons.find((el) => el.closest("button") && !el.closest("h2"));
    await userEvent.click(startButton!);

    await waitFor(() => {
      expect(screen.getByText("Starting...")).toBeInTheDocument();
    });
  });
});
