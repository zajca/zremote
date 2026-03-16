import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { ContextUsageBar } from "./ContextUsageBar";

describe("ContextUsageBar", () => {
  test("renders percentage text", () => {
    render(<ContextUsageBar used={50000} max={100000} />);
    expect(screen.getByText(/50\.0k \/ 100\.0k \(50%\)/)).toBeInTheDocument();
  });

  test("renders 0% when max is 0", () => {
    render(<ContextUsageBar used={0} max={0} />);
    expect(screen.getByText(/0%/)).toBeInTheDocument();
  });

  test("formats large token counts with M suffix", () => {
    render(<ContextUsageBar used={1500000} max={2000000} />);
    expect(screen.getByText(/1\.5M \/ 2\.0M/)).toBeInTheDocument();
  });

  test("formats small token counts without suffix", () => {
    render(<ContextUsageBar used={500} max={1000} />);
    // 1000 >= 1_000 so it renders as 1.0k
    expect(screen.getByText(/500 \/ 1\.0k/)).toBeInTheDocument();
  });

  test("shows warning icon at 85%+ usage", () => {
    const { container } = render(<ContextUsageBar used={90000} max={100000} />);
    // AlertTriangle SVG should be present
    const svg = container.querySelector("svg");
    expect(svg).toBeInTheDocument();
  });

  test("caps at 100%", () => {
    render(<ContextUsageBar used={150000} max={100000} />);
    expect(screen.getByText(/100%/)).toBeInTheDocument();
  });
});
