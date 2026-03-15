import { useCallback, useEffect, useRef, useState } from "react";
import { Input } from "./ui/Input";
import { Button } from "./ui/Button";

interface NewSessionDialogProps {
  open: boolean;
  onClose: () => void;
  onSubmit: (options: {
    name?: string;
    shell?: string;
    workingDir?: string;
  }) => void;
  defaultWorkingDir?: string;
}

const SHELL_OPTIONS = [
  { value: "", label: "Default (login shell)" },
  { value: "/bin/bash", label: "/bin/bash" },
  { value: "/bin/zsh", label: "/bin/zsh" },
  { value: "/bin/sh", label: "/bin/sh" },
  { value: "custom", label: "Custom..." },
];

export function NewSessionDialog({
  open,
  onClose,
  onSubmit,
  defaultWorkingDir,
}: NewSessionDialogProps) {
  const [name, setName] = useState("");
  const [shellOption, setShellOption] = useState("");
  const [customShell, setCustomShell] = useState("");
  const [workingDir, setWorkingDir] = useState("");
  const nameRef = useRef<HTMLInputElement>(null);

  // Reset form state when dialog opens
  useEffect(() => {
    if (open) {
      setName("");
      setShellOption("");
      setCustomShell("");
      setWorkingDir(defaultWorkingDir ?? "");
    }
  }, [open, defaultWorkingDir]);

  // Autofocus name input
  useEffect(() => {
    if (open) {
      // Small delay to ensure the dialog is rendered
      const timer = setTimeout(() => nameRef.current?.focus(), 50);
      return () => clearTimeout(timer);
    }
  }, [open]);

  const handleSubmit = useCallback(() => {
    const shell =
      shellOption === "custom"
        ? customShell || undefined
        : shellOption || undefined;
    onSubmit({
      name: name.trim() || undefined,
      shell,
      workingDir: workingDir.trim() || undefined,
    });
  }, [name, shellOption, customShell, workingDir, onSubmit]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      } else if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        handleSubmit();
      }
    },
    [onClose, handleSubmit],
  );

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
      onKeyDown={handleKeyDown}
    >
      <div
        className="w-full max-w-md rounded-lg border border-border bg-bg-primary p-6 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="mb-4 text-lg font-semibold text-text-primary">
          New Session
        </h2>

        <div className="flex flex-col gap-4">
          <Input
            ref={nameRef}
            label="Name"
            placeholder="e.g. deploy-prod"
            value={name}
            onChange={(e) => setName(e.target.value)}
          />

          <div className="flex flex-col gap-1.5">
            <label className="text-xs font-medium text-text-secondary">
              Shell
            </label>
            <select
              value={shellOption}
              onChange={(e) => setShellOption(e.target.value)}
              className="h-8 rounded-md border border-border bg-bg-tertiary px-3 text-sm text-text-primary transition-colors duration-150 focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none"
            >
              {SHELL_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
          </div>

          {shellOption === "custom" && (
            <Input
              label="Custom shell path"
              placeholder="/usr/local/bin/fish"
              value={customShell}
              onChange={(e) => setCustomShell(e.target.value)}
            />
          )}

          <Input
            label="Working Directory"
            placeholder="/home/user/project"
            value={workingDir}
            onChange={(e) => setWorkingDir(e.target.value)}
          />
        </div>

        <div className="mt-6 flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onClose}>
            Cancel
          </Button>
          <Button size="sm" onClick={handleSubmit}>
            Create
          </Button>
        </div>
      </div>
    </div>
  );
}
