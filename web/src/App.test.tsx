import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import App from "./App";

test("renders ZRemote heading in sidebar", async () => {
  render(<App />);
  expect(await screen.findByText("ZRemote")).toBeInTheDocument();
});

test("renders welcome page by default", async () => {
  render(<App />);
  expect(
    await screen.findByText("Welcome to ZRemote"),
  ).toBeInTheDocument();
});
