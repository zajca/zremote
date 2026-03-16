import { useCallback, useEffect, useState } from "react";
import { X } from "lucide-react";

interface ToastMessage {
  id: number;
  message: string;
  type: "error" | "info" | "success";
}

let nextId = 0;
const listeners: Set<(msg: ToastMessage) => void> = new Set();

export function showToast(message: string, type: "error" | "info" | "success" = "error") {
  const toast: ToastMessage = { id: nextId++, message, type };
  for (const listener of listeners) {
    listener(toast);
  }
}

export function ToastContainer() {
  const [toasts, setToasts] = useState<ToastMessage[]>([]);

  useEffect(() => {
    const listener = (msg: ToastMessage) => {
      setToasts((prev) => [...prev, msg]);
    };
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
    };
  }, []);

  const dismiss = useCallback((id: number) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  // Auto-dismiss: 4s for success, 8s for others
  useEffect(() => {
    if (toasts.length === 0) return;
    const oldest = toasts[0];
    if (!oldest) return;
    const delay = oldest.type === "success" ? 4000 : 8000;
    const timer = setTimeout(() => dismiss(oldest.id), delay);
    return () => clearTimeout(timer);
  }, [toasts, dismiss]);

  if (toasts.length === 0) return null;

  return (
    <div className="fixed right-4 bottom-4 z-50 flex flex-col gap-2">
      {toasts.map((toast) => (
        <div
          key={toast.id}
          className={`flex items-start gap-3 rounded-lg border bg-bg-secondary p-3 shadow-lg ${
            toast.type === "error"
              ? "border-l-4 border-status-error/40 border-l-status-error"
              : toast.type === "success"
                ? "border-l-4 border-status-online/40 border-l-status-online"
                : "border-border"
          }`}
          style={{ minWidth: 280, maxWidth: 420 }}
        >
          <span className="flex-1 text-sm text-text-primary">
            {toast.message}
          </span>
          <button
            onClick={() => dismiss(toast.id)}
            className="text-text-tertiary transition-colors hover:text-text-primary"
          >
            <X size={14} />
          </button>
        </div>
      ))}
    </div>
  );
}
