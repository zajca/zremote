import { useEffect, useRef } from "react";

export function useDoubleShift(callback: () => void) {
  const lastShiftRef = useRef(0);

  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key !== "Shift") return;
      if (e.repeat) return;
      if (e.ctrlKey || e.metaKey || e.altKey) return;

      const target = e.target as HTMLElement | null;
      if (target) {
        if (target instanceof HTMLInputElement) return;
        if (target instanceof HTMLTextAreaElement && !target.closest?.(".xterm")) return;
        if (target.isContentEditable) return;
      }

      const now = Date.now();
      if (now - lastShiftRef.current < 300) {
        lastShiftRef.current = 0;
        callback();
      } else {
        lastShiftRef.current = now;
      }
    }

    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [callback]);
}
