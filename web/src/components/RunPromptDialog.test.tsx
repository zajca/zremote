import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { RunPromptDialog } from "./RunPromptDialog";
import { usePendingPasteStore } from "../stores/pending-paste-store";
import type { PromptTemplate } from "../types/prompt";

const simpleTemplate: PromptTemplate = {
  name: "Fix Bug",
  description: "Fix a bug in the codebase",
  icon: "bug",
  body: "Fix the bug: {{description}}",
  inputs: [
    {
      name: "description",
      label: "Bug description",
      input_type: "text",
      placeholder: "Describe the bug...",
    },
  ],
  default_mode: "paste_to_terminal",
};

const multiInputTemplate: PromptTemplate = {
  name: "Review Code",
  icon: "code",
  body: "Review {{file}} with focus on {{area}}",
  inputs: [
    {
      name: "file",
      label: "File path",
      input_type: "text",
      placeholder: "/src/main.ts",
      required: true,
    },
    {
      name: "area",
      label: "Focus area",
      input_type: "select",
      options: ["security", "performance", "readability"],
      required: true,
    },
    {
      name: "notes",
      label: "Additional notes",
      input_type: "multiline",
      placeholder: "Any additional context...",
      required: false,
    },
  ],
  default_mode: "claude_session",
  model: "opus",
};

const defaultProps = {
  projectId: "proj-1",
  projectPath: "/home/user/project",
  hostId: "host-1",
  projectName: "test-project",
  onClose: vi.fn(),
};

beforeEach(() => {
  vi.restoreAllMocks();
  usePendingPasteStore.setState({ pendingPaste: null });
  global.fetch = vi.fn().mockResolvedValue({
    ok: true,
    text: async () => JSON.stringify({ prompt: "resolved prompt text" }),
  });
});

function renderDialog(
  template: PromptTemplate = simpleTemplate,
  props: Partial<typeof defaultProps> = {},
) {
  return render(
    <MemoryRouter>
      <RunPromptDialog template={template} {...defaultProps} {...props} />
    </MemoryRouter>,
  );
}

describe("RunPromptDialog", () => {
  test("renders template name as title", () => {
    renderDialog();
    expect(screen.getByRole("heading", { name: "Fix Bug" })).toBeInTheDocument();
  });

  test("renders template description", () => {
    renderDialog();
    expect(screen.getByText("Fix a bug in the codebase")).toBeInTheDocument();
  });

  test("renders project name", () => {
    renderDialog();
    expect(screen.getByText("Project: test-project")).toBeInTheDocument();
  });

  test("renders text input for text type", () => {
    renderDialog();
    expect(screen.getByPlaceholderText("Describe the bug...")).toBeInTheDocument();
  });

  test("renders select input for select type", () => {
    renderDialog(multiInputTemplate);
    expect(screen.getByText("security")).toBeInTheDocument();
    expect(screen.getByText("performance")).toBeInTheDocument();
    expect(screen.getByText("readability")).toBeInTheDocument();
  });

  test("renders textarea for multiline type", () => {
    renderDialog(multiInputTemplate);
    expect(
      screen.getByPlaceholderText("Any additional context..."),
    ).toBeInTheDocument();
  });

  test("renders required asterisk on required fields", () => {
    renderDialog();
    expect(screen.getByText("Bug description *")).toBeInTheDocument();
  });

  test("does not show asterisk on optional fields", () => {
    renderDialog(multiInputTemplate);
    expect(screen.getByText("Additional notes")).toBeInTheDocument();
  });

  test("renders execution mode toggle", () => {
    renderDialog();
    expect(screen.getByText("Paste to terminal")).toBeInTheDocument();
    expect(screen.getByText("Start Claude session")).toBeInTheDocument();
  });

  test("defaults to template default_mode", () => {
    renderDialog();
    const pasteBtn = screen.getByText("Paste to terminal").closest("button")!;
    expect(pasteBtn).toHaveClass("bg-accent");
  });

  test("defaults to claude_session mode when template specifies it", () => {
    renderDialog(multiInputTemplate);
    const claudeBtn = screen.getByText("Start Claude session").closest("button")!;
    expect(claudeBtn).toHaveClass("bg-accent");
  });

  test("shows model selector only in claude_session mode", async () => {
    renderDialog();
    // Default is paste_to_terminal, no model selector
    expect(screen.queryByText("Sonnet")).not.toBeInTheDocument();

    // Switch to claude_session
    await userEvent.click(screen.getByText("Start Claude session"));
    expect(screen.getByText("Sonnet")).toBeInTheDocument();
    expect(screen.getByText("Opus")).toBeInTheDocument();
    expect(screen.getByText("Haiku")).toBeInTheDocument();
  });

  test("defaults model to template.model when in claude_session mode", () => {
    renderDialog(multiInputTemplate);
    const opusBtn = screen.getByText("Opus").closest("button")!;
    expect(opusBtn).toHaveClass("bg-accent");
  });

  test("renders Cancel and Run prompt buttons", () => {
    renderDialog();
    expect(screen.getByText("Cancel")).toBeInTheDocument();
    expect(screen.getByText("Run prompt")).toBeInTheDocument();
  });

  test("calls onClose when Cancel is clicked", async () => {
    const onClose = vi.fn();
    renderDialog(simpleTemplate, { onClose });
    await userEvent.click(screen.getByText("Cancel"));
    expect(onClose).toHaveBeenCalled();
  });

  test("calls onClose when backdrop is clicked", async () => {
    const onClose = vi.fn();
    renderDialog(simpleTemplate, { onClose });
    const backdrop = screen.getByRole("heading", { name: "Fix Bug" }).closest(".fixed");
    await userEvent.click(backdrop!);
    expect(onClose).toHaveBeenCalled();
  });

  test("shows validation error for empty required field", async () => {
    renderDialog();
    await userEvent.click(screen.getByText("Run prompt"));
    expect(screen.getByText('Field "Bug description" is required')).toBeInTheDocument();
  });

  test("allows typing in text input", async () => {
    renderDialog();
    const input = screen.getByPlaceholderText("Describe the bug...");
    await userEvent.type(input, "Something is broken");
    expect(input).toHaveValue("Something is broken");
  });

  test("can switch execution mode", async () => {
    renderDialog();
    const claudeBtn = screen.getByText("Start Claude session");
    await userEvent.click(claudeBtn);
    expect(claudeBtn.closest("button")).toHaveClass("bg-accent");
  });

  test("shows error on API failure", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 500,
      text: async () => "Server error",
      statusText: "Internal Server Error",
    });

    renderDialog();
    const input = screen.getByPlaceholderText("Describe the bug...");
    await userEvent.type(input, "bug details");
    await userEvent.click(screen.getByText("Run prompt"));

    await waitFor(() => {
      expect(screen.getByText("Server error")).toBeInTheDocument();
    });
  });

  test("shows Running... state when submitting", async () => {
    global.fetch = vi.fn().mockImplementation(() => new Promise(() => {}));

    renderDialog();
    const input = screen.getByPlaceholderText("Describe the bug...");
    await userEvent.type(input, "bug details");
    await userEvent.click(screen.getByText("Run prompt"));

    await waitFor(() => {
      expect(screen.getByText("Running...")).toBeInTheDocument();
    });
  });

  test("paste mode: sets pending paste for current session", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      text: async () => JSON.stringify({ prompt: "resolved text" }),
    });

    const onClose = vi.fn();
    renderDialog(simpleTemplate, { onClose, currentSessionId: "sess-123" } as never);
    const input = screen.getByPlaceholderText("Describe the bug...");
    await userEvent.type(input, "fix this");
    await userEvent.click(screen.getByText("Run prompt"));

    await waitFor(() => {
      expect(onClose).toHaveBeenCalled();
    });

    // Pending paste should be set for the session (consumed later by Terminal)
    const paste = usePendingPasteStore.getState().pendingPaste;
    expect(paste).toEqual({ sessionId: "sess-123", data: "resolved text" });
  });

  test("renders default values from template inputs", () => {
    const template: PromptTemplate = {
      name: "Test",
      body: "{{field}}",
      inputs: [
        {
          name: "field",
          label: "Field",
          input_type: "text",
          default: "default value",
        },
      ],
    };
    renderDialog(template);
    const input = screen.getByDisplayValue("default value");
    expect(input).toBeInTheDocument();
  });

  test("renders close button with aria-label", () => {
    renderDialog();
    expect(screen.getByLabelText("Close")).toBeInTheDocument();
  });

  test("renders Execution mode label", () => {
    renderDialog();
    expect(screen.getByText("Execution mode")).toBeInTheDocument();
  });
});
