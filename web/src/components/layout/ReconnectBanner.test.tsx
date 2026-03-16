import { render, screen, act } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import {
  ReconnectBanner,
  dispatchWsDisconnected,
  dispatchWsReconnected,
} from "./ReconnectBanner";

describe("ReconnectBanner", () => {
  test("is hidden by default", () => {
    render(<ReconnectBanner />);
    expect(screen.queryByText("Reconnecting...")).not.toBeInTheDocument();
  });

  test("shows when browser goes offline", () => {
    render(<ReconnectBanner />);
    act(() => {
      window.dispatchEvent(new Event("offline"));
    });
    expect(screen.getByText("Reconnecting...")).toBeInTheDocument();
  });

  test("hides when browser comes online", () => {
    render(<ReconnectBanner />);
    act(() => {
      window.dispatchEvent(new Event("offline"));
    });
    expect(screen.getByText("Reconnecting...")).toBeInTheDocument();

    act(() => {
      window.dispatchEvent(new Event("online"));
    });
    expect(screen.queryByText("Reconnecting...")).not.toBeInTheDocument();
  });

  test("shows on WS disconnect event", () => {
    render(<ReconnectBanner />);
    act(() => {
      dispatchWsDisconnected();
    });
    expect(screen.getByText("Reconnecting...")).toBeInTheDocument();
  });

  test("hides on WS reconnect event", () => {
    render(<ReconnectBanner />);
    act(() => {
      dispatchWsDisconnected();
    });
    act(() => {
      dispatchWsReconnected();
    });
    expect(screen.queryByText("Reconnecting...")).not.toBeInTheDocument();
  });
});
