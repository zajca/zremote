import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryRouter } from "react-router";
import { ActionInputDialog } from "./ActionInputDialog";
import type { ProjectAction } from "../../lib/api";

// Mock navigate
const mockNavigate = vi.fn();
vi.mock("react-router", async () => {
  const actual = await vi.importActual("react-router");
  return {
    ...actual,
    useNavigate: () => mockNavigate,
  };
});

// Mock api
const mockRunAction = vi.fn();
const mockResolveActionInputs = vi.fn();
const mockWorktrees = vi.fn();

vi.mock("../../lib/api", async () => {
  const actual = await vi.importActual("../../lib/api");
  return {
    ...actual,
    api: {
      ...((actual as Record<string, unknown>).api ?? {}),
      projects: {
        runAction: (...args: unknown[]) => mockRunAction(...args),
        resolveActionInputs: (...args: unknown[]) => mockResolveActionInputs(...args),
        worktrees: (...args: unknown[]) => mockWorktrees(...args),
      },
    },
  };
});

vi.mock("../layout/Toast", () => ({
  showToast: vi.fn(),
}));

beforeEach(() => {
  vi.restoreAllMocks();
  mockRunAction.mockResolvedValue({ session_id: "sess-1", action: "test", command: "echo", working_dir: "/", status: "active", pid: 1 });
  mockResolveActionInputs.mockResolvedValue({ inputs: [] });
  mockWorktrees.mockResolvedValue([]);
});

const textAction: ProjectAction = {
  name: "deploy",
  command: "deploy --env {{env}} --tag {{tag}}",
  description: "Deploy to environment",
  env: {},
  worktree_scoped: false,
  inputs: [
    {
      name: "env",
      label: "Environment",
      input_type: "text",
      placeholder: "e.g. production",
      required: true,
    },
    {
      name: "tag",
      label: "Tag",
      input_type: "text",
      placeholder: "e.g. v1.0.0",
      required: false,
    },
  ],
};

const selectAction: ProjectAction = {
  name: "build",
  command: "make {{target}}",
  env: {},
  worktree_scoped: false,
  inputs: [
    {
      name: "target",
      label: "Target",
      input_type: "select",
      options: ["release", "debug", "test"],
      required: true,
    },
  ],
};

const multilineAction: ProjectAction = {
  name: "run-script",
  command: "bash -c '{{script}}'",
  env: {},
  worktree_scoped: false,
  inputs: [
    {
      name: "script",
      label: "Script",
      input_type: "multiline",
      placeholder: "Enter script...",
    },
  ],
};

const scriptedSelectAction: ProjectAction = {
  name: "switch-branch",
  command: "git checkout {{branch}}",
  env: {},
  worktree_scoped: false,
  inputs: [
    {
      name: "branch",
      label: "Branch",
      input_type: "select",
      script: "git branch --format='%(refname:short)'",
      required: true,
    },
  ],
};

const worktreeTemplateAction: ProjectAction = {
  name: "wt-test",
  command: "cd {{worktree_path}} && cargo test",
  env: {},
  worktree_scoped: true,
  inputs: [
    {
      name: "filter",
      label: "Test filter",
      input_type: "text",
      placeholder: "filter pattern",
      required: false,
    },
  ],
};

const defaultProps = {
  projectId: "proj-1",
  hostId: "host-1",
  onClose: vi.fn(),
};

function renderDialog(action: ProjectAction = textAction, props: Partial<typeof defaultProps> = {}) {
  return render(
    <MemoryRouter>
      <ActionInputDialog action={action} {...defaultProps} {...props} />
    </MemoryRouter>,
  );
}

describe("ActionInputDialog", () => {
  test("renders action name as title", () => {
    renderDialog();
    expect(screen.getByRole("heading", { name: "deploy" })).toBeInTheDocument();
  });

  test("renders action description", () => {
    renderDialog();
    expect(screen.getByText("Deploy to environment")).toBeInTheDocument();
  });

  test("renders text inputs", () => {
    renderDialog();
    expect(screen.getByPlaceholderText("e.g. production")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("e.g. v1.0.0")).toBeInTheDocument();
  });

  test("renders select input with static options", () => {
    renderDialog(selectAction);
    expect(screen.getByText("release")).toBeInTheDocument();
    expect(screen.getByText("debug")).toBeInTheDocument();
    expect(screen.getByText("test")).toBeInTheDocument();
  });

  test("renders multiline textarea", () => {
    renderDialog(multilineAction);
    expect(screen.getByPlaceholderText("Enter script...")).toBeInTheDocument();
  });

  test("renders required asterisk on required fields", () => {
    renderDialog();
    expect(screen.getByText("Environment *")).toBeInTheDocument();
  });

  test("does not show asterisk on optional fields", () => {
    renderDialog();
    expect(screen.getByText("Tag")).toBeInTheDocument();
  });

  test("shows loading skeleton during script resolution", async () => {
    mockResolveActionInputs.mockImplementation(() => new Promise(() => {})); // never resolves
    renderDialog(scriptedSelectAction);
    expect(screen.getByTestId("skeleton-branch")).toBeInTheDocument();
  });

  test("shows resolved scripted options after loading", async () => {
    mockResolveActionInputs.mockResolvedValue({
      inputs: [
        {
          name: "branch",
          options: [
            { value: "main", label: "main" },
            { value: "develop", label: "develop" },
          ],
        },
      ],
    });

    renderDialog(scriptedSelectAction);

    await waitFor(() => {
      expect(screen.getByText("main")).toBeInTheDocument();
      expect(screen.getByText("develop")).toBeInTheDocument();
    });
  });

  test("handles script error per-input", async () => {
    mockResolveActionInputs.mockRejectedValue(new Error("Script failed"));
    renderDialog(scriptedSelectAction);

    await waitFor(() => {
      expect(screen.getByText("Failed to load options")).toBeInTheDocument();
      expect(screen.getByTestId("error-branch")).toBeInTheDocument();
    });
  });

  test("shows empty state when script returns no options", async () => {
    mockResolveActionInputs.mockResolvedValue({
      inputs: [{ name: "branch", options: [] }],
    });

    renderDialog(scriptedSelectAction);

    await waitFor(() => {
      expect(screen.getByText("No options available")).toBeInTheDocument();
    });
  });

  test("validates required fields on submit", async () => {
    const user = userEvent.setup();
    renderDialog();

    await user.click(screen.getByText("Run action"));
    expect(screen.getByText('Field "Environment" is required')).toBeInTheDocument();
  });

  test("submits with collected values", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    renderDialog(textAction, { onClose });

    await user.type(screen.getByPlaceholderText("e.g. production"), "staging");
    await user.type(screen.getByPlaceholderText("e.g. v1.0.0"), "v2.0");
    await user.click(screen.getByText("Run action"));

    await waitFor(() => {
      expect(mockRunAction).toHaveBeenCalledWith("proj-1", "deploy", {
        inputs: { env: "staging", tag: "v2.0" },
      });
      expect(onClose).toHaveBeenCalled();
    });
  });

  test("renders worktree selector when command has {{worktree_path}} and no worktreePath prop", async () => {
    mockWorktrees.mockResolvedValue([
      { id: "wt-1", path: "/repo/wt1", name: "wt1", git_branch: "feature-a", host_id: "host-1" },
    ]);

    renderDialog(worktreeTemplateAction);

    // Should show worktree skeleton initially, then selector
    await waitFor(() => {
      expect(screen.getByText("Worktree *")).toBeInTheDocument();
    });

    await waitFor(() => {
      expect(screen.getByText("feature-a")).toBeInTheDocument();
    });
  });

  test("does not show worktree selector when worktreePath prop provided", () => {
    renderDialog(worktreeTemplateAction, { worktreePath: "/some/path" } as never);
    expect(screen.queryByText("Worktree *")).not.toBeInTheDocument();
  });

  test("Escape closes dialog", () => {
    const onClose = vi.fn();
    renderDialog(textAction, { onClose });

    // The onKeyDown handler is on the overlay div
    const overlay = screen.getByRole("heading", { name: "deploy" }).closest(".fixed")!;
    fireEvent.keyDown(overlay, { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });

  test("live command preview updates when values change", async () => {
    const user = userEvent.setup();
    renderDialog();

    // Initially shows template vars
    expect(screen.getByText("deploy --env {{env}} --tag {{tag}}")).toBeInTheDocument();

    // Type in environment
    await user.type(screen.getByPlaceholderText("e.g. production"), "prod");

    // Preview should update
    expect(screen.getByText("deploy --env prod --tag {{tag}}")).toBeInTheDocument();
  });

  test("renders close button with aria-label", () => {
    renderDialog();
    expect(screen.getByLabelText("Close")).toBeInTheDocument();
  });

  test("renders Cancel and Run action buttons", () => {
    renderDialog();
    expect(screen.getByText("Cancel")).toBeInTheDocument();
    expect(screen.getByText("Run action")).toBeInTheDocument();
  });

  test("calls onClose when Cancel is clicked", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    renderDialog(textAction, { onClose });
    await user.click(screen.getByText("Cancel"));
    expect(onClose).toHaveBeenCalled();
  });

  test("shows Running... state when submitting", async () => {
    const user = userEvent.setup();
    mockRunAction.mockImplementation(() => new Promise(() => {})); // never resolves
    renderDialog();

    await user.type(screen.getByPlaceholderText("e.g. production"), "prod");
    await user.click(screen.getByText("Run action"));

    await waitFor(() => {
      expect(screen.getByText("Running...")).toBeInTheDocument();
    });
  });

  test("renders default values from inputs", () => {
    const action: ProjectAction = {
      name: "test",
      command: "echo {{msg}}",
      env: {},
      worktree_scoped: false,
      inputs: [
        {
          name: "msg",
          label: "Message",
          input_type: "text",
          default: "hello world",
        },
      ],
    };
    renderDialog(action);
    expect(screen.getByDisplayValue("hello world")).toBeInTheDocument();
  });

  test("renders command preview section", () => {
    renderDialog();
    expect(screen.getByText("Command preview")).toBeInTheDocument();
  });

  test("shows retry button for script errors", async () => {
    mockResolveActionInputs.mockRejectedValue(new Error("Script failed"));
    renderDialog(scriptedSelectAction);

    await waitFor(() => {
      expect(screen.getByLabelText("Retry loading Branch")).toBeInTheDocument();
    });
  });
});
