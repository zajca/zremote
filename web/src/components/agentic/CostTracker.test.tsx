import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { CostTracker } from "./CostTracker";

describe("CostTracker", () => {
  test("renders cost in dollar format", () => {
    render(
      <CostTracker costUsd={1.5} tokensIn={10000} tokensOut={5000} />,
    );
    expect(screen.getByText("$1.50")).toBeInTheDocument();
  });

  test("renders token counts", () => {
    render(
      <CostTracker costUsd={0.5} tokensIn={10000} tokensOut={5000} />,
    );
    expect(screen.getByText("10.0k in / 5.0k out")).toBeInTheDocument();
  });

  test("compact mode shows only cost with tooltip", () => {
    render(
      <CostTracker costUsd={0.25} tokensIn={15000} tokensOut={5000} compact />,
    );
    const el = screen.getByText("$0.25");
    expect(el).toBeInTheDocument();
    expect(el.getAttribute("title")).toBe("15.0k in / 5.0k out");
  });

  test("formats large token counts with M suffix", () => {
    render(
      <CostTracker costUsd={10} tokensIn={1500000} tokensOut={500000} />,
    );
    expect(screen.getByText("1.5M in / 500.0k out")).toBeInTheDocument();
  });

  test("formats small token counts without suffix", () => {
    render(
      <CostTracker costUsd={0.01} tokensIn={500} tokensOut={200} />,
    );
    expect(screen.getByText("500 in / 200 out")).toBeInTheDocument();
  });
});
