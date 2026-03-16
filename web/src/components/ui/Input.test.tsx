import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test } from "vitest";
import { Input } from "./Input";

describe("Input", () => {
  test("renders an input element", () => {
    render(<Input />);
    expect(screen.getByRole("textbox")).toBeInTheDocument();
  });

  test("renders with label", () => {
    render(<Input label="Name" id="name-input" />);
    expect(screen.getByLabelText("Name")).toBeInTheDocument();
  });

  test("renders without label", () => {
    render(<Input placeholder="Type here" />);
    expect(screen.queryByRole("label")).not.toBeInTheDocument();
    expect(screen.getByPlaceholderText("Type here")).toBeInTheDocument();
  });

  test("accepts user input", async () => {
    render(<Input placeholder="Type" />);
    const input = screen.getByPlaceholderText("Type");
    await userEvent.type(input, "hello");
    expect(input).toHaveValue("hello");
  });

  test("passes through HTML input attributes", () => {
    render(<Input type="password" name="pass" disabled />);
    // password type hides from textbox role, so query via DOM
    const el = document.querySelector('input[name="pass"]') as HTMLInputElement;
    expect(el).toBeDisabled();
    expect(el.type).toBe("password");
  });

  test("applies custom className", () => {
    render(<Input className="my-class" />);
    expect(screen.getByRole("textbox").className).toContain("my-class");
  });
});
