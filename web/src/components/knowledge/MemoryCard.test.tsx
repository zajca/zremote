import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";
import { MemoryCard } from "./MemoryCard";
import type { KnowledgeMemory } from "../../types/knowledge";

const mockMemory: KnowledgeMemory = {
  id: "mem-1",
  project_id: "proj-1",
  loop_id: null,
  key: "error-handling",
  content: "Always use Result type for error handling.",
  category: "pattern",
  confidence: 0.85,
  created_at: "2026-03-15T10:00:00Z",
  updated_at: "2026-03-15T12:00:00Z",
};

describe("MemoryCard", () => {
  test("renders memory key", () => {
    render(<MemoryCard memory={mockMemory} onDelete={vi.fn()} onUpdate={vi.fn()} />);
    expect(screen.getByText("error-handling")).toBeInTheDocument();
  });

  test("renders memory content", () => {
    render(<MemoryCard memory={mockMemory} onDelete={vi.fn()} onUpdate={vi.fn()} />);
    expect(
      screen.getByText("Always use Result type for error handling."),
    ).toBeInTheDocument();
  });

  test("renders category badge", () => {
    render(<MemoryCard memory={mockMemory} onDelete={vi.fn()} onUpdate={vi.fn()} />);
    expect(screen.getByText("pattern")).toBeInTheDocument();
  });

  test("renders confidence percentage", () => {
    render(<MemoryCard memory={mockMemory} onDelete={vi.fn()} onUpdate={vi.fn()} />);
    expect(screen.getByText("85%")).toBeInTheDocument();
  });

  test("calls onDelete when Delete is clicked", async () => {
    const onDelete = vi.fn();
    render(<MemoryCard memory={mockMemory} onDelete={onDelete} onUpdate={vi.fn()} />);
    await userEvent.click(screen.getByText("Delete"));
    expect(onDelete).toHaveBeenCalledOnce();
  });

  test("shows edit textarea when Edit is clicked", async () => {
    render(<MemoryCard memory={mockMemory} onDelete={vi.fn()} onUpdate={vi.fn()} />);
    await userEvent.click(screen.getByText("Edit"));
    expect(screen.getByRole("textbox")).toBeInTheDocument();
    expect(screen.getByText("Save")).toBeInTheDocument();
    expect(screen.getByText("Cancel")).toBeInTheDocument();
  });

  test("calls onUpdate when saving edited content", async () => {
    const onUpdate = vi.fn();
    render(<MemoryCard memory={mockMemory} onDelete={vi.fn()} onUpdate={onUpdate} />);
    await userEvent.click(screen.getByText("Edit"));
    const textarea = screen.getByRole("textbox");
    await userEvent.clear(textarea);
    await userEvent.type(textarea, "Updated content");
    await userEvent.click(screen.getByText("Save"));
    expect(onUpdate).toHaveBeenCalledWith({ content: "Updated content" });
  });
});
