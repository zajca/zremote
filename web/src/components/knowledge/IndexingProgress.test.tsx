import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { IndexingProgress } from "./IndexingProgress";
import type { IndexingProgress as IndexingProgressType } from "../../types/knowledge";

describe("IndexingProgress", () => {
  test("renders queued status", () => {
    const progress: IndexingProgressType = {
      project_id: "proj-1",
      project_path: "/home/user/project",
      status: "queued",
      files_processed: 0,
      files_total: 100,
    };
    render(<IndexingProgress progress={progress} />);
    expect(screen.getByText(/Queued/)).toBeInTheDocument();
    expect(screen.getByText("0/100 files")).toBeInTheDocument();
  });

  test("renders in-progress percentage", () => {
    const progress: IndexingProgressType = {
      project_id: "proj-1",
      project_path: "/home/user/project",
      status: "in_progress",
      files_processed: 50,
      files_total: 100,
    };
    render(<IndexingProgress progress={progress} />);
    expect(screen.getByText(/50%/)).toBeInTheDocument();
    expect(screen.getByText("50/100 files")).toBeInTheDocument();
  });

  test("renders 0% when total is 0", () => {
    const progress: IndexingProgressType = {
      project_id: "proj-1",
      project_path: "/path",
      status: "in_progress",
      files_processed: 0,
      files_total: 0,
    };
    render(<IndexingProgress progress={progress} />);
    expect(screen.getByText(/0%/)).toBeInTheDocument();
  });
});
