import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { StartClaudeDialog } from "./StartClaudeDialog";

beforeEach(() => {
  vi.restoreAllMocks();
  global.fetch = vi.fn().mockImplementation((url: string) => {
    if (url.includes("/api/claude-sessions")) {
      return Promise.resolve({ ok: true, json: async () => [] });
    }
    return Promise.resolve({ ok: true, json: async () => ({}) });
  });
});

describe("StartClaudeDialog", () => {
  test("renders dialog title", () => {
    render(
      <MemoryRouter>
        <StartClaudeDialog
          projectName="my-project"
          projectPath="/home/user/project"
          hostId="host-1"
          onClose={vi.fn()}
        />
      </MemoryRouter>,
    );
    expect(screen.getByRole("heading", { name: "Start Claude" })).toBeInTheDocument();
  });

  test("renders project name", () => {
    render(
      <MemoryRouter>
        <StartClaudeDialog
          projectName="my-project"
          projectPath="/path"
          hostId="host-1"
          onClose={vi.fn()}
        />
      </MemoryRouter>,
    );
    expect(screen.getByText("Project: my-project")).toBeInTheDocument();
  });

  test("renders model selector buttons", () => {
    render(
      <MemoryRouter>
        <StartClaudeDialog
          projectName="test"
          projectPath="/path"
          hostId="host-1"
          onClose={vi.fn()}
        />
      </MemoryRouter>,
    );
    expect(screen.getByText("Sonnet")).toBeInTheDocument();
    expect(screen.getByText("Opus")).toBeInTheDocument();
    expect(screen.getByText("Haiku")).toBeInTheDocument();
  });

  test("renders prompt textarea", () => {
    render(
      <MemoryRouter>
        <StartClaudeDialog
          projectName="test"
          projectPath="/path"
          hostId="host-1"
          onClose={vi.fn()}
        />
      </MemoryRouter>,
    );
    expect(screen.getByPlaceholderText("What should Claude do?")).toBeInTheDocument();
  });

  test("renders Cancel and Start Claude buttons", () => {
    render(
      <MemoryRouter>
        <StartClaudeDialog
          projectName="test"
          projectPath="/path"
          hostId="host-1"
          onClose={vi.fn()}
        />
      </MemoryRouter>,
    );
    expect(screen.getByText("Cancel")).toBeInTheDocument();
    // "Start Claude" appears both as heading and button
    expect(screen.getAllByText("Start Claude").length).toBe(2);
  });

  test("calls onClose when Cancel is clicked", async () => {
    const onClose = vi.fn();
    render(
      <MemoryRouter>
        <StartClaudeDialog
          projectName="test"
          projectPath="/path"
          hostId="host-1"
          onClose={onClose}
        />
      </MemoryRouter>,
    );
    await userEvent.click(screen.getByText("Cancel"));
    expect(onClose).toHaveBeenCalled();
  });

  test("renders Options toggle", () => {
    render(
      <MemoryRouter>
        <StartClaudeDialog
          projectName="test"
          projectPath="/path"
          hostId="host-1"
          onClose={vi.fn()}
        />
      </MemoryRouter>,
    );
    expect(screen.getByText("Options")).toBeInTheDocument();
  });
});
