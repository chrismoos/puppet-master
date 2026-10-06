import { useState, type DragEvent, type KeyboardEvent } from "react";
import type { SavedWorkspace } from "../api/workspaces";
import type { Route } from "../router";
import { navigate } from "../router";

const TAB_DRAG_TYPE = "application/x-puppet-master-top-tab";

export function moveTab(keys: number[], source: number, target: number | null): number[] {
  if (source === target) return keys;
  if (target !== null && !keys.includes(target)) return keys;
  return moveTabToIndex(keys, source, target === null ? keys.length : keys.indexOf(target));
}

export function moveTabToIndex(keys: number[], source: number, insertionIndex: number): number[] {
  const sourceIndex = keys.indexOf(source);
  if (sourceIndex < 0) return keys;
  const next = keys.filter((key) => key !== source);
  const adjustedIndex = sourceIndex < insertionIndex ? insertionIndex - 1 : insertionIndex;
  next.splice(Math.max(0, Math.min(adjustedIndex, next.length)), 0, source);
  return next;
}

export function tabInsertionIndex(pointerX: number, left: number, width: number, tabIndex: number): number {
  return pointerX < left + width / 2 ? tabIndex : tabIndex + 1;
}

export function requestWorkspaceDelete(event: { preventDefault: () => void; stopPropagation: () => void }, workspace: SavedWorkspace, onDelete: (workspace: SavedWorkspace) => void): void {
  event.preventDefault();
  event.stopPropagation();
  onDelete(workspace);
}

export function requestWorkspaceRename(event: { stopPropagation: () => void }, workspace: SavedWorkspace, onRename: (workspace: SavedWorkspace) => void): void {
  event.stopPropagation();
  onRename(workspace);
}

export function removeWorkspaceFromTabs(workspaces: SavedWorkspace[], workspaceId: number): SavedWorkspace[] {
  const ordered = [...workspaces].sort((first, second) => first.position - second.position || first.id - second.id);
  const removedPosition = ordered.findIndex((workspace) => workspace.id === workspaceId);
  if (removedPosition < 0) return workspaces;
  return ordered
    .filter((workspace) => workspace.id !== workspaceId)
    .map((workspace, position) => ({ ...workspace, position }));
}

export function adjacentWorkspaceTab(index: number, count: number, key: string): number | null {
  if (count === 0) return null;
  switch (key) {
    case "ArrowLeft": return (index - 1 + count) % count;
    case "ArrowRight": return (index + 1) % count;
    case "Home": return 0;
    case "End": return count - 1;
    default: return null;
  }
}

export function WorkspaceTabs({
  route,
  workspaces,
  onCreate,
  onDelete,
  onRename,
  onReorder,
}: {
  route: Route;
  workspaces: SavedWorkspace[];
  onCreate: () => void;
  onDelete: (workspace: SavedWorkspace) => void;
  onRename: (workspace: SavedWorkspace) => void;
  onReorder: (workspaceIds: number[]) => void;
}) {
  const [dragged, setDragged] = useState<number | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);
  const ordered = [...workspaces].sort((first, second) => first.position - second.position || first.id - second.id);
  const keys = ordered.map((workspace) => workspace.id);

  const drop = (event: DragEvent, insertionIndex: number) => {
    event.preventDefault();
    const data = event.dataTransfer.getData(TAB_DRAG_TYPE);
    const stored = data === "" ? null : Number(data);
    const source = stored !== null && Number.isSafeInteger(stored) ? stored : dragged;
    if (source === null) return;
    const next = moveTabToIndex(keys, source, insertionIndex);
    onReorder(next);
    setDragged(null);
    setDropIndex(null);
  };

  const dragProps = (key: number, index: number) => ({
    draggable: true,
    onDragStart: (event: DragEvent<HTMLDivElement>) => {
      if ((event.target as HTMLElement).closest(".workspace-tab-delete")) {
        event.preventDefault();
        return;
      }
      setDragged(key);
      event.dataTransfer.effectAllowed = "move";
      event.dataTransfer.setData(TAB_DRAG_TYPE, String(key));
    },
    onDragEnd: () => {
      setDragged(null);
      setDropIndex(null);
    },
    onDragOver: (event: DragEvent<HTMLDivElement>) => {
      event.preventDefault();
      event.dataTransfer.dropEffect = "move";
      const bounds = event.currentTarget.getBoundingClientRect();
      setDropIndex(tabInsertionIndex(event.clientX, bounds.left, bounds.width, index));
    },
    onDrop: (event: DragEvent<HTMLDivElement>) => {
      const bounds = event.currentTarget.getBoundingClientRect();
      drop(event, tabInsertionIndex(event.clientX, bounds.left, bounds.width, index));
    },
  });

  const navigateByKey = (event: KeyboardEvent<HTMLElement>, index: number) => {
    const targetIndex = adjacentWorkspaceTab(index, keys.length, event.key);
    if (targetIndex === null) return;
    event.preventDefault();
    const target = event.currentTarget.parentElement?.parentElement
      ?.querySelectorAll<HTMLButtonElement>(".workspace-tab-main")[targetIndex];
    target?.focus();
    target?.click();
  };

  return (
    <nav className={`workspace-tabs ${dragged !== null ? "is-reordering" : ""}`} aria-label="saved workspaces" onDragLeave={(event) => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDropIndex(null); }}>
      {keys.map((key, index) => {
        const workspace = ordered.find((item) => item.id === key);
        if (!workspace) return null;
        return (
          <div
            key={key}
            className={`workspace-tab ${route.name === "workspace" && route.id === workspace.id ? "is-active" : ""} ${dragged === key ? "is-dragging" : ""} ${dropIndex === index ? "is-drop-before" : ""}`}
            {...dragProps(key, index)}
          >
            <button type="button" className="workspace-tab-main" aria-current={route.name === "workspace" && route.id === workspace.id ? "page" : undefined} onKeyDown={(event) => navigateByKey(event, index)} onClick={() => navigate(`/workspace/${workspace.id}`)}>
              <span aria-hidden="true">▦</span>
              <span><strong title="Double-click to rename workspace" onDoubleClick={(event) => requestWorkspaceRename(event, workspace, onRename)}>{workspace.name}</strong><small>saved workspace</small></span>
            </button>
            <button type="button" className="workspace-tab-delete" title={`Delete ${workspace.name}`} aria-label={`Delete ${workspace.name}`} onClick={(event) => requestWorkspaceDelete(event, workspace, onDelete)}>×</button>
          </div>
        );
      })}
      <div
        className={`workspace-tab-end-drop ${dropIndex === keys.length ? "is-active" : ""}`}
        aria-hidden="true"
        onDragOver={(event) => { event.preventDefault(); event.dataTransfer.dropEffect = "move"; setDropIndex(keys.length); }}
        onDrop={(event) => drop(event, keys.length)}
      />
      <button
        type="button"
        className="workspace-new-tab"
        title="new workspace"
        onClick={onCreate}
      >＋</button>
    </nav>
  );
}
