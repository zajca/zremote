import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import App from "./App";

test("renders MyRemote heading in sidebar", async () => {
  render(<App />);
  expect(await screen.findByText("MyRemote")).toBeInTheDocument();
});

test("renders welcome page by default", async () => {
  render(<App />);
  expect(
    await screen.findByText("Welcome to MyRemote"),
  ).toBeInTheDocument();
});
