import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { TranscriptView } from "./TranscriptView";
import type { TranscriptEntry } from "../../types/agentic";

function makeEntry(overrides: Partial<TranscriptEntry> = {}): TranscriptEntry {
  return {
    id: 1,
    loop_id: "loop-1",
    role: "assistant",
    content: "Hello, how can I help?",
    tool_call_id: null,
    timestamp: "2026-03-16T10:00:00Z",
    ...overrides,
  };
}

describe("TranscriptView", () => {
  test("shows empty state when no entries", () => {
    render(<TranscriptView entries={[]} />);
    expect(screen.getByText("No transcript entries yet")).toBeInTheDocument();
  });

  test("renders assistant entry with label", () => {
    const entries = [makeEntry({ role: "assistant", content: "I can help with that." })];
    render(<TranscriptView entries={entries} />);
    expect(screen.getByText("Assistant")).toBeInTheDocument();
    expect(screen.getByText("I can help with that.")).toBeInTheDocument();
  });

  test("renders user entry with label", () => {
    const entries = [makeEntry({ id: 2, role: "user", content: "Fix this bug" })];
    render(<TranscriptView entries={entries} />);
    expect(screen.getByText("You")).toBeInTheDocument();
    expect(screen.getByText("Fix this bug")).toBeInTheDocument();
  });

  test("renders tool entry with label", () => {
    const entries = [makeEntry({ id: 3, role: "tool", content: "file contents..." })];
    render(<TranscriptView entries={entries} />);
    expect(screen.getByText("Tool")).toBeInTheDocument();
    expect(screen.getByText("file contents...")).toBeInTheDocument();
  });

  test("renders system entry without label", () => {
    const entries = [makeEntry({ id: 4, role: "system", content: "Context updated" })];
    render(<TranscriptView entries={entries} />);
    expect(screen.queryByText("System")).not.toBeInTheDocument();
    expect(screen.getByText("Context updated")).toBeInTheDocument();
  });

  test("renders multiple entries", () => {
    const entries = [
      makeEntry({ id: 1, role: "user", content: "Hello" }),
      makeEntry({ id: 2, role: "assistant", content: "Hi there" }),
      makeEntry({ id: 3, role: "tool", content: "result: ok" }),
    ];
    render(<TranscriptView entries={entries} />);
    expect(screen.getByText("Hello")).toBeInTheDocument();
    expect(screen.getByText("Hi there")).toBeInTheDocument();
    expect(screen.getByText("result: ok")).toBeInTheDocument();
  });
});
