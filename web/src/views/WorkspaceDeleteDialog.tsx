import { ConfirmDialog } from "./ConfirmDialog";

export function WorkspaceDeleteDialog({
  workspaceName,
  onClose,
  onConfirm,
}: {
  workspaceName: string;
  onClose: () => void;
  onConfirm: () => Promise<void>;
}) {
  return (
    <ConfirmDialog
      title="delete workspace"
      titleId="workspace-delete-title"
      className="workspace-delete-modal"
      confirmLabel="delete workspace"
      busyLabel="deleting…"
      onClose={onClose}
      onConfirm={onConfirm}
    >
      <p>Delete “{workspaceName}”?</p>
      <p className="muted-line">Its saved layout will be removed. Agent sessions and terminals will keep running.</p>
    </ConfirmDialog>
  );
}
