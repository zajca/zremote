import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { Command } from "cmdk";
import {
  BarChart3,
  Clock,
  Laptop,
  Search,
  Settings,
} from "lucide-react";
import { useHosts } from "../../hooks/useHosts";

export function CommandPalette() {
  const [open, setOpen] = useState(false);
  const navigate = useNavigate();
  const { hosts } = useHosts();

  // Toggle on Cmd+K / Ctrl+K
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setOpen((prev) => !prev);
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);

  const runAction = useCallback(
    (path: string) => {
      setOpen(false);
      void navigate(path);
    },
    [navigate],
  );

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[20vh]">
      {/* Backdrop */}
      <div
        className="fixed inset-0 bg-black/50"
        onClick={() => setOpen(false)}
      />

      <Command
        className="relative w-full max-w-lg overflow-hidden rounded-xl border border-border bg-bg-secondary shadow-2xl"
        loop
      >
        <div className="flex items-center gap-2 border-b border-border px-3">
          <Search size={14} className="text-text-tertiary" />
          <Command.Input
            placeholder="Search commands..."
            className="h-10 w-full bg-transparent text-sm text-text-primary placeholder:text-text-tertiary focus:outline-none"
          />
        </div>

        <Command.List className="max-h-80 overflow-auto p-2">
          <Command.Empty className="px-3 py-6 text-center text-sm text-text-tertiary">
            No results found
          </Command.Empty>

          <Command.Group
            heading="Navigation"
            className="[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-text-tertiary"
          >
            <CommandItem
              icon={<BarChart3 size={14} />}
              onSelect={() => runAction("/analytics")}
            >
              Open Analytics
            </CommandItem>
            <CommandItem
              icon={<Clock size={14} />}
              onSelect={() => runAction("/history")}
            >
              Open History
            </CommandItem>
            <CommandItem
              icon={<Settings size={14} />}
              onSelect={() => runAction("/settings")}
            >
              Open Settings
            </CommandItem>
          </Command.Group>

          {hosts.length > 0 && (
            <Command.Group
              heading="Hosts"
              className="[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-text-tertiary"
            >
              {hosts.map((host) => (
                <CommandItem
                  key={host.id}
                  icon={<Laptop size={14} />}
                  onSelect={() => runAction(`/hosts/${host.id}`)}
                >
                  Go to {host.hostname}
                </CommandItem>
              ))}
            </Command.Group>
          )}
        </Command.List>

        <div className="flex items-center justify-between border-t border-border px-3 py-1.5 text-xs text-text-tertiary">
          <span>Navigate with arrow keys</span>
          <div className="flex items-center gap-2">
            <kbd className="rounded bg-bg-tertiary px-1.5 py-0.5 font-mono text-[10px]">
              Esc
            </kbd>
            <span>to close</span>
          </div>
        </div>
      </Command>
    </div>
  );
}

function CommandItem({
  children,
  icon,
  onSelect,
}: {
  children: React.ReactNode;
  icon: React.ReactNode;
  onSelect: () => void;
}) {
  return (
    <Command.Item
      onSelect={onSelect}
      className="flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-sm text-text-secondary transition-colors duration-75 data-[selected=true]:bg-bg-hover data-[selected=true]:text-text-primary"
    >
      {icon}
      {children}
    </Command.Item>
  );
}
