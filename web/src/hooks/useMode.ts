import { type ReactNode, useState, useEffect, createContext, useContext, createElement } from "react";
import { detectMode, type AppMode } from "../lib/connection";

interface ModeContextValue {
  mode: AppMode;
  isLocal: boolean;
}

const ModeContext = createContext<ModeContextValue>({
  mode: "server",
  isLocal: false,
});

export function ModeProvider({ children }: { children: ReactNode }) {
  const [mode, setMode] = useState<AppMode>("server");

  useEffect(() => {
    void detectMode().then(setMode);
  }, []);

  return createElement(
    ModeContext.Provider,
    { value: { mode, isLocal: mode === "local" } },
    children,
  );
}

export function useMode(): ModeContextValue {
  return useContext(ModeContext);
}
