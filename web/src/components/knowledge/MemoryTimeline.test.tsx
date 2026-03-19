import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { MemoryTimeline } from "./MemoryTimeline";

vi.mock("../../stores/knowledge-store", () => ({
  useKnowledgeStore: () => ({
    memoriesByProject: {},
    fetchMemories: vi.fn(),
    deleteMemory: vi.fn(),
    updateMemory: vi.fn(),
  }),
}));

describe("MemoryTimeline", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  test("renders category filter buttons", () => {
    render(<MemoryTimeline projectId="proj-1" />);
    expect(screen.getByText("All")).toBeInTheDocument();
    expect(screen.getByText("Patterns")).toBeInTheDocument();
    expect(screen.getByText("Decisions")).toBeInTheDocument();
    expect(screen.getByText("Pitfalls")).toBeInTheDocument();
    expect(screen.getByText("Preferences")).toBeInTheDocument();
    expect(screen.getByText("Architecture")).toBeInTheDocument();
    expect(screen.getByText("Conventions")).toBeInTheDocument();
  });

  test("shows empty state when no memories", () => {
    render(<MemoryTimeline projectId="proj-1" />);
    expect(screen.getByText("No memories extracted yet")).toBeInTheDocument();
  });
});
