import { renderHook, act } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach, afterEach } from "vitest";
import { useDoubleShift } from "./useDoubleShift";

describe("useDoubleShift", () => {
  let callback: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    callback = vi.fn();
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  function fireShift(options?: Partial<KeyboardEvent>) {
    const event = new KeyboardEvent("keydown", {
      key: "Shift",
      bubbles: true,
      ...options,
    });
    document.dispatchEvent(event);
  }

  test("fires callback on double-shift within 300ms", () => {
    renderHook(() => useDoubleShift(callback));

    act(() => {
      vi.setSystemTime(1000);
      fireShift();
    });

    act(() => {
      vi.setSystemTime(1200); // 200ms later
      fireShift();
    });

    expect(callback).toHaveBeenCalledTimes(1);
  });

  test("does NOT fire on single shift", () => {
    renderHook(() => useDoubleShift(callback));

    act(() => {
      vi.setSystemTime(1000);
      fireShift();
    });

    expect(callback).not.toHaveBeenCalled();
  });

  test("does NOT fire if > 300ms apart", () => {
    renderHook(() => useDoubleShift(callback));

    act(() => {
      vi.setSystemTime(1000);
      fireShift();
    });

    act(() => {
      vi.setSystemTime(1400); // 400ms later
      fireShift();
    });

    expect(callback).not.toHaveBeenCalled();
  });

  test("does NOT fire on held key (repeat=true)", () => {
    renderHook(() => useDoubleShift(callback));

    act(() => {
      vi.setSystemTime(1000);
      fireShift();
    });

    act(() => {
      vi.setSystemTime(1100);
      fireShift({ repeat: true });
    });

    expect(callback).not.toHaveBeenCalled();
  });

  test("does NOT fire with modifier keys (ctrl+shift)", () => {
    renderHook(() => useDoubleShift(callback));

    act(() => {
      vi.setSystemTime(1000);
      fireShift();
    });

    act(() => {
      vi.setSystemTime(1100);
      fireShift({ ctrlKey: true });
    });

    expect(callback).not.toHaveBeenCalled();
  });

  test("does NOT fire with meta+shift", () => {
    renderHook(() => useDoubleShift(callback));

    act(() => {
      vi.setSystemTime(1000);
      fireShift();
    });

    act(() => {
      vi.setSystemTime(1100);
      fireShift({ metaKey: true });
    });

    expect(callback).not.toHaveBeenCalled();
  });

  test("does NOT fire with alt+shift", () => {
    renderHook(() => useDoubleShift(callback));

    act(() => {
      vi.setSystemTime(1000);
      fireShift();
    });

    act(() => {
      vi.setSystemTime(1100);
      fireShift({ altKey: true });
    });

    expect(callback).not.toHaveBeenCalled();
  });

  test("does NOT fire when focused in input element", () => {
    renderHook(() => useDoubleShift(callback));

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();

    act(() => {
      vi.setSystemTime(1000);
      const event = new KeyboardEvent("keydown", {
        key: "Shift",
        bubbles: true,
      });
      Object.defineProperty(event, "target", { value: input });
      document.dispatchEvent(event);
    });

    act(() => {
      vi.setSystemTime(1100);
      const event = new KeyboardEvent("keydown", {
        key: "Shift",
        bubbles: true,
      });
      Object.defineProperty(event, "target", { value: input });
      document.dispatchEvent(event);
    });

    expect(callback).not.toHaveBeenCalled();
    document.body.removeChild(input);
  });

  test("does NOT fire when focused in textarea element", () => {
    renderHook(() => useDoubleShift(callback));

    const textarea = document.createElement("textarea");
    document.body.appendChild(textarea);
    textarea.focus();

    act(() => {
      vi.setSystemTime(1000);
      const event = new KeyboardEvent("keydown", {
        key: "Shift",
        bubbles: true,
      });
      Object.defineProperty(event, "target", { value: textarea });
      document.dispatchEvent(event);
    });

    act(() => {
      vi.setSystemTime(1100);
      const event = new KeyboardEvent("keydown", {
        key: "Shift",
        bubbles: true,
      });
      Object.defineProperty(event, "target", { value: textarea });
      document.dispatchEvent(event);
    });

    expect(callback).not.toHaveBeenCalled();
    document.body.removeChild(textarea);
  });

  test("fires callback when focused in .xterm element", () => {
    renderHook(() => useDoubleShift(callback));

    const xtermDiv = document.createElement("div");
    xtermDiv.className = "xterm";
    const childSpan = document.createElement("span");
    xtermDiv.appendChild(childSpan);
    document.body.appendChild(xtermDiv);

    act(() => {
      vi.setSystemTime(1000);
      const event = new KeyboardEvent("keydown", {
        key: "Shift",
        bubbles: true,
      });
      Object.defineProperty(event, "target", { value: childSpan });
      document.dispatchEvent(event);
    });

    act(() => {
      vi.setSystemTime(1100);
      const event = new KeyboardEvent("keydown", {
        key: "Shift",
        bubbles: true,
      });
      Object.defineProperty(event, "target", { value: childSpan });
      document.dispatchEvent(event);
    });

    expect(callback).toHaveBeenCalledTimes(1);
    document.body.removeChild(xtermDiv);
  });
});
