import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { KnowledgePanel } from "./KnowledgePanel";

vi.mock("../../stores/knowledge-store", () => ({
  useKnowledgeStore: () => ({
    fetchStatus: vi.fn(),
    fetchMemories: vi.fn(),
    indexingProgress: {},
    statusByProject: {},
    memoriesByProject: {},
    controlService: vi.fn(),
    triggerIndex: vi.fn(),
    search: vi.fn(),
    searchResults: [],
    searchLoading: false,
    deleteMemory: vi.fn(),
    updateMemory: vi.fn(),
  }),
}));

describe("KnowledgePanel", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  test("renders Knowledge heading", () => {
    render(<KnowledgePanel projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("Knowledge")).toBeInTheDocument();
  });

  test("renders tab buttons", () => {
    render(<KnowledgePanel projectId="proj-1" hostId="host-1" />);
    expect(screen.getByText("Status")).toBeInTheDocument();
    expect(screen.getByText("Search")).toBeInTheDocument();
    expect(screen.getByText("Memories")).toBeInTheDocument();
    expect(screen.getByText("Instructions")).toBeInTheDocument();
  });

  test("switches tabs on click", async () => {
    render(<KnowledgePanel projectId="proj-1" hostId="host-1" />);
    await userEvent.click(screen.getByText("Search"));
    // Search tab should show the search interface
    expect(screen.getByPlaceholderText("Search project knowledge...")).toBeInTheDocument();
  });
});
