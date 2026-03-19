import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { KnowledgeStatus } from "./KnowledgeStatus";

const mockControlService = vi.fn().mockResolvedValue(undefined);
const mockTriggerIndex = vi.fn().mockResolvedValue(undefined);
const mockFetchStatus = vi.fn();

let mockStatusByProject: Record<string, unknown> = {};

vi.mock("../../stores/knowledge-store", () => ({
  useKnowledgeStore: () => ({
    statusByProject: mockStatusByProject,
    controlService: mockControlService,
    triggerIndex: mockTriggerIndex,
    fetchStatus: mockFetchStatus,
  }),
}));

beforeEach(() => {
  vi.restoreAllMocks();
  mockStatusByProject = {};
  mockControlService.mockResolvedValue(undefined);
  mockTriggerIndex.mockResolvedValue(undefined);
  global.fetch = vi.fn().mockResolvedValue({
    ok: true,
    json: async () => ({}),
    text: async () => "{}",
  });
});

describe("KnowledgeStatus", () => {
  test("shows not configured message when no status", () => {
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("not configured")).toBeInTheDocument();
  });

  test("shows Start Service button when no status", () => {
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("Start Service")).toBeInTheDocument();
  });

  test("shows OpenViking label", () => {
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("OpenViking")).toBeInTheDocument();
  });

  test("shows setup instructions when not configured", () => {
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("OpenViking is not configured on this host.")).toBeInTheDocument();
    expect(screen.getByText(/pip install openviking/)).toBeInTheDocument();
    expect(screen.getByText(/OPENVIKING_ENABLED=true/)).toBeInTheDocument();
    expect(screen.getByText(/OPENROUTER_API_KEY/)).toBeInTheDocument();
    expect(screen.getByText("Restart agent")).toBeInTheDocument();
  });

  test("shows Start Service button when status is stopped", () => {
    mockStatusByProject = {
      "proj-1": { status: "stopped" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("stopped")).toBeInTheDocument();
    expect(screen.getByText("Start Service")).toBeInTheDocument();
  });

  test("shows Start Service button when status is error", () => {
    mockStatusByProject = {
      "proj-1": { status: "error", last_error: "Connection failed" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("error")).toBeInTheDocument();
    expect(screen.getByText("Start Service")).toBeInTheDocument();
    expect(screen.getByText("Connection failed")).toBeInTheDocument();
  });

  test("shows ready-state buttons when status is ready", () => {
    mockStatusByProject = {
      "proj-1": { status: "ready" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("ready")).toBeInTheDocument();
    expect(screen.getByText("Stop")).toBeInTheDocument();
    expect(screen.getByText("Restart")).toBeInTheDocument();
    expect(screen.getByText("Index Project")).toBeInTheDocument();
    expect(screen.getByText("Force Reindex")).toBeInTheDocument();
    expect(screen.getByText("Bootstrap Knowledge")).toBeInTheDocument();
  });

  test("does not show Start Service when status is ready", () => {
    mockStatusByProject = {
      "proj-1": { status: "ready" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    expect(screen.queryByText("Start Service")).not.toBeInTheDocument();
  });

  test("shows version when available", () => {
    mockStatusByProject = {
      "proj-1": { status: "ready", openviking_version: "1.2.3" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("v1.2.3")).toBeInTheDocument();
  });

  test("clicking Start Service calls controlService with start", async () => {
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    await userEvent.click(screen.getByText("Start Service"));
    await waitFor(() => {
      expect(mockControlService).toHaveBeenCalledWith("host-1", "start");
    });
  });

  test("clicking Stop calls controlService with stop", async () => {
    mockStatusByProject = {
      "proj-1": { status: "ready" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    await userEvent.click(screen.getByText("Stop"));
    await waitFor(() => {
      expect(mockControlService).toHaveBeenCalledWith("host-1", "stop");
    });
  });

  test("clicking Restart calls controlService with restart", async () => {
    mockStatusByProject = {
      "proj-1": { status: "ready" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    await userEvent.click(screen.getByText("Restart"));
    await waitFor(() => {
      expect(mockControlService).toHaveBeenCalledWith("host-1", "restart");
    });
  });

  test("clicking Index Project calls triggerIndex", async () => {
    mockStatusByProject = {
      "proj-1": { status: "ready" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    await userEvent.click(screen.getByText("Index Project"));
    await waitFor(() => {
      expect(mockTriggerIndex).toHaveBeenCalledWith("proj-1");
    });
  });

  test("clicking Force Reindex calls triggerIndex with force=true", async () => {
    mockStatusByProject = {
      "proj-1": { status: "ready" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    await userEvent.click(screen.getByText("Force Reindex"));
    await waitFor(() => {
      expect(mockTriggerIndex).toHaveBeenCalledWith("proj-1", true);
    });
  });

  test("Bootstrap Knowledge button shows bootstrapping state", async () => {
    mockStatusByProject = {
      "proj-1": { status: "ready" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);

    await userEvent.click(screen.getByText("Bootstrap Knowledge"));

    await waitFor(() => {
      expect(screen.getByText("Bootstrapping...")).toBeInTheDocument();
    });
  });

  test("shows setup instructions when error includes 'not enabled'", () => {
    mockStatusByProject = {
      "proj-1": { status: "error", last_error: "OpenViking is not enabled on this host" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("OpenViking is not configured on this host.")).toBeInTheDocument();
  });

  test("shows indexing badge variant", () => {
    mockStatusByProject = {
      "proj-1": { status: "indexing" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("indexing")).toBeInTheDocument();
  });

  test("shows starting badge variant", () => {
    mockStatusByProject = {
      "proj-1": { status: "starting" },
    };
    render(<KnowledgeStatus projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("starting")).toBeInTheDocument();
  });
});
