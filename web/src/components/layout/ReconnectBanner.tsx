import { useEffect, useState } from "react";
import { WifiOff } from "lucide-react";

export function ReconnectBanner() {
  const [disconnected, setDisconnected] = useState(false);

  useEffect(() => {
    function onOffline() {
      setDisconnected(true);
    }
    function onOnline() {
      setDisconnected(false);
    }

    window.addEventListener("offline", onOffline);
    window.addEventListener("online", onOnline);

    // Also listen for custom WS disconnect events
    function onWsDisconnect() {
      setDisconnected(true);
    }
    function onWsReconnect() {
      setDisconnected(false);
    }
    window.addEventListener("zremote:ws-disconnected", onWsDisconnect);
    window.addEventListener("zremote:ws-reconnected", onWsReconnect);

    return () => {
      window.removeEventListener("offline", onOffline);
      window.removeEventListener("online", onOnline);
      window.removeEventListener("zremote:ws-disconnected", onWsDisconnect);
      window.removeEventListener("zremote:ws-reconnected", onWsReconnect);
    };
  }, []);

  if (!disconnected) return null;

  return (
    <div className="fixed top-0 right-0 left-0 z-50 flex items-center justify-center gap-2 bg-status-warning/15 py-1.5 text-xs font-medium text-status-warning">
      <WifiOff size={14} />
      Reconnecting...
    </div>
  );
}

// Utility to dispatch WS connection events from useRealtimeUpdates
export function dispatchWsDisconnected() {
  window.dispatchEvent(new Event("zremote:ws-disconnected"));
}

export function dispatchWsReconnected() {
  window.dispatchEvent(new Event("zremote:ws-reconnected"));
}
