import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { InstructionGenerator } from "./InstructionGenerator";

beforeEach(() => {
  vi.restoreAllMocks();
  const data = { content: "# Instructions\nDo this.", memories_used: 5 };
  global.fetch = vi.fn().mockResolvedValue({
    ok: true,
    text: async () => JSON.stringify(data),
    json: async () => data,
  });
});

describe("InstructionGenerator", () => {
  test("renders generate button", () => {
    render(<InstructionGenerator projectId="proj-1" />);
    expect(screen.getByText("Generate")).toBeInTheDocument();
  });

  test("renders description text", () => {
    render(<InstructionGenerator projectId="proj-1" />);
    expect(
      screen.getByText("Generate project instructions from extracted memories."),
    ).toBeInTheDocument();
  });

  test("shows generated content after clicking Generate", async () => {
    render(<InstructionGenerator projectId="proj-1" />);
    await userEvent.click(screen.getByText("Generate"));
    // Content renders inside a <pre> tag
    expect(await screen.findByText(/# Instructions/)).toBeInTheDocument();
    expect(screen.getByText("Based on 5 memories")).toBeInTheDocument();
  });
});
