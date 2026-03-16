import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { KnowledgeStatus } from "./KnowledgeStatus";

vi.mock("../../stores/knowledge-store", () => ({
  useKnowledgeStore: () => ({
    statusByProject: {},
    controlService: vi.fn(),
    triggerIndex: vi.fn(),
    fetchStatus: vi.fn(),
  }),
}));

describe("KnowledgeStatus", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

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
});
