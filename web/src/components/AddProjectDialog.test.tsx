import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { AddProjectDialog } from "./AddProjectDialog";

describe("AddProjectDialog", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      text: async () => "[]",
    });
  });

  test("renders nothing when not open", () => {
    const { container } = render(
      <AddProjectDialog hostId="host-1" open={false} onClose={vi.fn()} />,
    );
    expect(container.children.length).toBe(0);
  });

  test("renders with path input when open", () => {
    render(
      <AddProjectDialog hostId="host-1" open={true} onClose={vi.fn()} />,
    );
    expect(screen.getByRole("heading", { name: "Add Project" })).toBeInTheDocument();
    expect(
      screen.getByPlaceholderText("/home/user/my-project"),
    ).toBeInTheDocument();
  });

  test("browse panel toggles on button click", async () => {
    render(
      <AddProjectDialog hostId="host-1" open={true} onClose={vi.fn()} />,
    );

    const browseBtn = screen.getByText("Browse...");
    await userEvent.click(browseBtn);

    // Browse panel should show the path indicator
    await waitFor(() => {
      expect(screen.getByText("Close")).toBeInTheDocument();
    });

    // Toggle off
    await userEvent.click(screen.getByText("Close"));
    expect(screen.getByText("Browse...")).toBeInTheDocument();
  });

  test("add button disabled when path empty", () => {
    render(
      <AddProjectDialog hostId="host-1" open={true} onClose={vi.fn()} />,
    );

    const addBtn = screen.getByRole("button", { name: "Add Project" });
    expect(addBtn).toBeDisabled();
  });

  test("add button enabled when path entered", async () => {
    render(
      <AddProjectDialog hostId="host-1" open={true} onClose={vi.fn()} />,
    );

    const input = screen.getByPlaceholderText("/home/user/my-project");
    await userEvent.type(input, "/home/user/project");

    const addBtn = screen.getByRole("button", { name: "Add Project" });
    expect(addBtn).toBeEnabled();
  });

  test("calls API on submit", async () => {
    const onClose = vi.fn();
    const onProjectAdded = vi.fn();

    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      text: async () =>
        JSON.stringify({
          id: "proj-1",
          host_id: "host-1",
          path: "/home/user/project",
          name: "project",
          has_claude_config: false,
          project_type: "unknown",
          created_at: new Date().toISOString(),
          parent_project_id: null,
          git_branch: null,
          git_commit_hash: null,
          git_commit_message: null,
          git_is_dirty: false,
          git_ahead: 0,
          git_behind: 0,
          git_remotes: null,
          git_updated_at: null,
        }),
    });

    render(
      <AddProjectDialog
        hostId="host-1"
        open={true}
        onClose={onClose}
        onProjectAdded={onProjectAdded}
      />,
    );

    const input = screen.getByPlaceholderText("/home/user/my-project");
    await userEvent.type(input, "/home/user/project");
    await userEvent.click(screen.getByRole("button", { name: "Add Project" }));

    await waitFor(() => {
      expect(onProjectAdded).toHaveBeenCalled();
      expect(onClose).toHaveBeenCalled();
    });

    expect(global.fetch).toHaveBeenCalledWith(
      "/api/hosts/host-1/projects",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ path: "/home/user/project" }),
      }),
    );
  });

  test("shows error on 409 conflict", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 409,
      text: async () => "409 Conflict",
      statusText: "Conflict",
    });

    render(
      <AddProjectDialog hostId="host-1" open={true} onClose={vi.fn()} />,
    );

    const input = screen.getByPlaceholderText("/home/user/my-project");
    await userEvent.type(input, "/home/user/project");
    await userEvent.click(screen.getByRole("button", { name: "Add Project" }));

    await waitFor(() => {
      expect(screen.getByText("Project already added")).toBeInTheDocument();
    });
  });

  test("calls onClose when Cancel clicked", async () => {
    const onClose = vi.fn();
    render(
      <AddProjectDialog hostId="host-1" open={true} onClose={onClose} />,
    );

    await userEvent.click(screen.getByText("Cancel"));
    expect(onClose).toHaveBeenCalled();
  });

  test("calls onClose when X button clicked", async () => {
    const onClose = vi.fn();
    render(
      <AddProjectDialog hostId="host-1" open={true} onClose={onClose} />,
    );

    const closeButtons = screen
      .getAllByRole("button")
      .filter((btn) => btn.querySelector("svg"));
    await userEvent.click(closeButtons[0]);
    expect(onClose).toHaveBeenCalled();
  });
});
