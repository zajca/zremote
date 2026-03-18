import { renderHook } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import { useCommandPaletteContext } from "./useCommandPaletteContext";

let mockPathname = "/";

vi.mock("react-router", () => ({
  useLocation: () => ({ pathname: mockPathname }),
}));

describe("useCommandPaletteContext", () => {
  test("returns global for /", () => {
    mockPathname = "/";
    const { result } = renderHook(() => useCommandPaletteContext());
    expect(result.current).toEqual({ level: "global" });
  });

  test("returns global for /analytics", () => {
    mockPathname = "/analytics";
    const { result } = renderHook(() => useCommandPaletteContext());
    expect(result.current).toEqual({ level: "global" });
  });

  test("returns global for /history", () => {
    mockPathname = "/history";
    const { result } = renderHook(() => useCommandPaletteContext());
    expect(result.current).toEqual({ level: "global" });
  });

  test("returns global for /settings", () => {
    mockPathname = "/settings";
    const { result } = renderHook(() => useCommandPaletteContext());
    expect(result.current).toEqual({ level: "global" });
  });

  test("returns host level for /hosts/abc", () => {
    mockPathname = "/hosts/abc";
    const { result } = renderHook(() => useCommandPaletteContext());
    expect(result.current).toEqual({ level: "host", hostId: "abc" });
  });

  test("returns session level for /hosts/abc/sessions/def", () => {
    mockPathname = "/hosts/abc/sessions/def";
    const { result } = renderHook(() => useCommandPaletteContext());
    expect(result.current).toEqual({
      level: "session",
      hostId: "abc",
      sessionId: "def",
    });
  });

  test("returns project level for /projects/xyz", () => {
    mockPathname = "/projects/xyz";
    const { result } = renderHook(() => useCommandPaletteContext());
    expect(result.current).toEqual({
      level: "project",
      projectId: "xyz",
    });
  });
});
