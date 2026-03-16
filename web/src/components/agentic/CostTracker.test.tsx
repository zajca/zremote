import { render, screen } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { CostTracker } from "./CostTracker";

describe("CostTracker", () => {
  test("renders cost in dollar format", () => {
    render(
      <CostTracker costUsd={1.5} tokensIn={10000} tokensOut={5000} model="sonnet" />,
    );
    expect(screen.getByText("$1.50")).toBeInTheDocument();
  });

  test("renders token counts", () => {
    render(
      <CostTracker costUsd={0.5} tokensIn={10000} tokensOut={5000} model="sonnet" />,
    );
    expect(screen.getByText("10.0k in / 5.0k out")).toBeInTheDocument();
  });

  test("renders model name", () => {
    render(
      <CostTracker costUsd={0} tokensIn={0} tokensOut={0} model="opus" />,
    );
    expect(screen.getByText("opus")).toBeInTheDocument();
  });

  test("does not render model when null", () => {
    const { container } = render(
      <CostTracker costUsd={0} tokensIn={0} tokensOut={0} model={null} />,
    );
    // When model is null, only one separator "|" should be present (between cost and tokens)
    // but no model text
    const spans = container.querySelectorAll("span");
    const texts = Array.from(spans).map((s) => s.textContent);
    expect(texts.some((t) => t === "opus" || t === "sonnet")).toBe(false);
  });

  test("formats large token counts with M suffix", () => {
    render(
      <CostTracker
        costUsd={10}
        tokensIn={1500000}
        tokensOut={500000}
        model={null}
      />,
    );
    expect(screen.getByText("1.5M in / 500.0k out")).toBeInTheDocument();
  });

  test("formats small token counts without suffix", () => {
    render(
      <CostTracker costUsd={0.01} tokensIn={500} tokensOut={200} model={null} />,
    );
    expect(screen.getByText("500 in / 200 out")).toBeInTheDocument();
  });
});
