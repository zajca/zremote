import { render, screen, waitFor } from "@testing-library/react";
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
    expect(await screen.findByText(/# Instructions/)).toBeInTheDocument();
    expect(screen.getByText("Based on 5 memories")).toBeInTheDocument();
  });

  test("shows Generating... while loading", async () => {
    // Make fetch hang
    global.fetch = vi.fn().mockImplementation(
      () => new Promise(() => {}),
    );

    render(<InstructionGenerator projectId="proj-1" />);
    await userEvent.click(screen.getByText("Generate"));

    expect(screen.getByText("Generating...")).toBeInTheDocument();
    // Generate button should be disabled
    const genBtn = screen.getByText("Generating...").closest("button");
    expect(genBtn).toBeDisabled();
  });

  test("shows Copy to clipboard button after generating", async () => {
    render(<InstructionGenerator projectId="proj-1" />);
    await userEvent.click(screen.getByText("Generate"));
    await waitFor(() => {
      expect(screen.getByText("Copy to clipboard")).toBeInTheDocument();
    });
  });

  test("shows Copied! after clicking copy", async () => {
    // Mock clipboard
    Object.assign(navigator, {
      clipboard: { writeText: vi.fn().mockResolvedValue(undefined) },
    });

    render(<InstructionGenerator projectId="proj-1" />);
    await userEvent.click(screen.getByText("Generate"));
    await waitFor(() => {
      expect(screen.getByText("Copy to clipboard")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("Copy to clipboard"));
    expect(screen.getByText("Copied!")).toBeInTheDocument();
  });

  test("shows Write to CLAUDE.md button after generating", async () => {
    render(<InstructionGenerator projectId="proj-1" />);
    await userEvent.click(screen.getByText("Generate"));
    await waitFor(() => {
      expect(screen.getByText("Write to CLAUDE.md")).toBeInTheDocument();
    });
  });

  test("shows success message after writing to CLAUDE.md", async () => {
    const generateData = { content: "# Instructions", memories_used: 3 };
    const writeData = { written: true, bytes: 512 };
    let callCount = 0;
    global.fetch = vi.fn().mockImplementation(() => {
      callCount++;
      if (callCount === 1) {
        return Promise.resolve({
          ok: true,
          text: async () => JSON.stringify(generateData),
          json: async () => generateData,
        });
      }
      return Promise.resolve({
        ok: true,
        text: async () => JSON.stringify(writeData),
        json: async () => writeData,
      });
    });

    render(<InstructionGenerator projectId="proj-1" />);
    await userEvent.click(screen.getByText("Generate"));

    await waitFor(() => {
      expect(screen.getByText("Write to CLAUDE.md")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("Write to CLAUDE.md"));

    await waitFor(() => {
      expect(screen.getByText("Written to CLAUDE.md (512 bytes)")).toBeInTheDocument();
    });
  });

  test("shows Writing... while writing to CLAUDE.md", async () => {
    const generateData = { content: "# Instructions", memories_used: 3 };
    let callCount = 0;
    global.fetch = vi.fn().mockImplementation(() => {
      callCount++;
      if (callCount === 1) {
        return Promise.resolve({
          ok: true,
          text: async () => JSON.stringify(generateData),
          json: async () => generateData,
        });
      }
      return new Promise(() => {}); // hang on write
    });

    render(<InstructionGenerator projectId="proj-1" />);
    await userEvent.click(screen.getByText("Generate"));

    await waitFor(() => {
      expect(screen.getByText("Write to CLAUDE.md")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("Write to CLAUDE.md"));

    await waitFor(() => {
      expect(screen.getByText("Writing...")).toBeInTheDocument();
    });
  });

  test("shows error message when writing to CLAUDE.md fails", async () => {
    const generateData = { content: "# Instructions", memories_used: 3 };
    let callCount = 0;
    global.fetch = vi.fn().mockImplementation(() => {
      callCount++;
      if (callCount === 1) {
        return Promise.resolve({
          ok: true,
          text: async () => JSON.stringify(generateData),
          json: async () => generateData,
        });
      }
      return Promise.resolve({
        ok: false,
        status: 500,
        text: async () => "Write failed",
        statusText: "Internal Server Error",
      });
    });

    render(<InstructionGenerator projectId="proj-1" />);
    await userEvent.click(screen.getByText("Generate"));

    await waitFor(() => {
      expect(screen.getByText("Write to CLAUDE.md")).toBeInTheDocument();
    });

    await userEvent.click(screen.getByText("Write to CLAUDE.md"));

    await waitFor(() => {
      expect(screen.getByText(/Failed/)).toBeInTheDocument();
    });
  });

  test("does not show content section before generating", () => {
    render(<InstructionGenerator projectId="proj-1" />);
    expect(screen.queryByText("Copy to clipboard")).not.toBeInTheDocument();
    expect(screen.queryByText("Write to CLAUDE.md")).not.toBeInTheDocument();
  });
});
