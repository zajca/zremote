import { Folder, FolderOpen, Loader2, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import type { DirectoryEntry } from "../lib/api";
import { Button } from "./ui/Button";
import { Input } from "./ui/Input";

interface AddProjectDialogProps {
  hostId: string;
  open: boolean;
  onClose: () => void;
  onProjectAdded?: () => void;
}

export function AddProjectDialog({
  hostId,
  open,
  onClose,
  onProjectAdded,
}: AddProjectDialogProps) {
  const [path, setPath] = useState("");
  const [browsing, setBrowsing] = useState(false);
  const [browsePath, setBrowsePath] = useState("/");
  const [entries, setEntries] = useState<DirectoryEntry[]>([]);
  const [browseLoading, setBrowseLoading] = useState(false);
  const [browseError, setBrowseError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // Reset state when dialog opens
  useEffect(() => {
    if (open) {
      setPath("");
      setBrowsing(false);
      setBrowsePath("/");
      setEntries([]);
      setBrowseLoading(false);
      setBrowseError(null);
      setSubmitting(false);
      setError(null);
      const timer = setTimeout(() => inputRef.current?.focus(), 50);
      return () => clearTimeout(timer);
    }
  }, [open]);

  // Fetch directory listing when browsePath changes
  useEffect(() => {
    if (!browsing) return;
    setBrowseLoading(true);
    setBrowseError(null);
    void api.projects
      .browse(hostId, browsePath)
      .then((result) => {
        setEntries(Array.isArray(result) ? result : []);
        setBrowseLoading(false);
      })
      .catch((err) => {
        setBrowseError(
          err instanceof Error ? err.message : "Failed to browse directory",
        );
        setEntries([]);
        setBrowseLoading(false);
      });
  }, [hostId, browsePath, browsing]);

  const handleToggleBrowse = useCallback(() => {
    setBrowsing((prev) => !prev);
  }, []);

  const handleNavigate = useCallback(
    (name: string) => {
      const newPath =
        browsePath === "/"
          ? `/${name}`
          : `${browsePath}/${name}`;
      setBrowsePath(newPath);
      setPath(newPath);
    },
    [browsePath],
  );

  const handleNavigateUp = useCallback(() => {
    const parent = browsePath.replace(/\/[^/]+$/, "") || "/";
    setBrowsePath(parent);
    setPath(parent);
  }, [browsePath]);

  const handleSubmit = useCallback(async () => {
    if (!path.trim() || submitting) return;
    setError(null);
    setSubmitting(true);

    try {
      await api.projects.add(hostId, path.trim());
      onProjectAdded?.();
      onClose();
    } catch (err) {
      if (err instanceof Error && err.message.includes("409")) {
        setError("Project already added");
      } else {
        setError(
          err instanceof Error ? err.message : "Failed to add project",
        );
      }
    } finally {
      setSubmitting(false);
    }
  }, [path, submitting, hostId, onProjectAdded, onClose]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      } else if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        void handleSubmit();
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
        className="max-h-[80vh] w-full max-w-lg overflow-y-auto rounded-lg border border-border bg-bg-primary p-6 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <FolderOpen size={18} className="text-accent" />
            <h2 className="text-lg font-semibold text-text-primary">
              Add Project
            </h2>
          </div>
          <button
            onClick={onClose}
            className="rounded p-1 text-text-tertiary transition-colors hover:bg-bg-hover hover:text-text-primary"
          >
            <X size={16} />
          </button>
        </div>

        <div className="flex flex-col gap-4">
          <div className="flex items-end gap-2">
            <div className="flex-1">
              <Input
                ref={inputRef}
                label="Project path"
                placeholder="/home/user/my-project"
                value={path}
                onChange={(e) => setPath(e.target.value)}
              />
            </div>
            <Button
              variant="secondary"
              size="sm"
              onClick={handleToggleBrowse}
              className="shrink-0"
            >
              {browsing ? "Close" : "Browse..."}
            </Button>
          </div>

          {browsing && (
            <div className="rounded-md border border-border bg-bg-secondary">
              <div className="flex items-center gap-2 border-b border-border px-3 py-2">
                <Folder size={12} className="text-text-tertiary" />
                <span className="truncate text-xs text-text-secondary">
                  {browsePath}
                </span>
              </div>
              {browseLoading ? (
                <div className="flex items-center justify-center py-6">
                  <Loader2
                    size={16}
                    className="animate-spin text-text-tertiary"
                  />
                </div>
              ) : browseError ? (
                <div className="px-3 py-4 text-center text-xs text-status-error">
                  {browseError}
                </div>
              ) : (
                <div className="max-h-48 overflow-y-auto">
                  {browsePath !== "/" && (
                    <button
                      onClick={handleNavigateUp}
                      className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-text-secondary transition-colors hover:bg-bg-hover"
                    >
                      <Folder size={12} className="text-text-tertiary" />
                      ..
                    </button>
                  )}
                  {entries
                    .filter((e) => e.is_dir)
                    .map((entry) => (
                      <button
                        key={entry.name}
                        onClick={() => handleNavigate(entry.name)}
                        className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-text-primary transition-colors hover:bg-bg-hover"
                      >
                        <Folder size={12} className="text-accent" />
                        {entry.name}
                        {entry.is_symlink && (
                          <span className="text-[10px] text-text-tertiary">
                            symlink
                          </span>
                        )}
                      </button>
                    ))}
                  {entries.filter((e) => e.is_dir).length === 0 &&
                    browsePath === "/" && (
                      <div className="px-3 py-4 text-center text-xs text-text-tertiary">
                        No directories found
                      </div>
                    )}
                </div>
              )}
            </div>
          )}

          {error && (
            <div className="rounded-md border border-status-error/30 bg-status-error/10 px-3 py-2 text-xs text-status-error">
              {error}
            </div>
          )}
        </div>

        <div className="mt-6 flex justify-end gap-2">
          <Button
            variant="ghost"
            size="sm"
            onClick={onClose}
            disabled={submitting}
          >
            Cancel
          </Button>
          <Button
            size="sm"
            onClick={() => void handleSubmit()}
            disabled={!path.trim() || submitting}
          >
            {submitting ? (
              <>
                <Loader2 size={14} className="animate-spin" />
                Adding...
              </>
            ) : (
              "Add Project"
            )}
          </Button>
        </div>
      </div>
    </div>
  );
}
