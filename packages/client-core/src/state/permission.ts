import { PermissionMode } from "../gen/pm/v1/pm_pb";

/**
 * Resolves the effective mode for a spawn by cascading spawn override →
 * project override → bucket default, treating UNSPECIFIED as "inherit the
 * level above". Buckets never carry UNSPECIFIED, so DEFAULT is only a
 * defensive floor.
 */
export function resolvePermissionMode(
  spawn: PermissionMode,
  project: PermissionMode,
  bucket: PermissionMode,
): PermissionMode {
  if (spawn !== PermissionMode.UNSPECIFIED) return spawn;
  if (project !== PermissionMode.UNSPECIFIED) return project;
  if (bucket !== PermissionMode.UNSPECIFIED) return bucket;
  return PermissionMode.DEFAULT;
}

export function permissionModeLabel(mode: PermissionMode): string {
  switch (mode) {
    case PermissionMode.DEFAULT:
      return "default";
    case PermissionMode.AUTO:
      return "auto";
    case PermissionMode.BYPASS:
      return "bypass";
    case PermissionMode.UNSPECIFIED:
      return "inherit";
  }
}

export interface PermissionTag {
  label: string;
  className: string;
}

/** A sidebar tag for a resolved mode, or null when the mode is the quiet default. */
export function permissionTag(mode: PermissionMode): PermissionTag | null {
  switch (mode) {
    case PermissionMode.AUTO:
      return { label: "auto", className: "pm-tag-auto" };
    case PermissionMode.BYPASS:
      return { label: "bypass", className: "pm-tag-bypass" };
    case PermissionMode.DEFAULT:
    case PermissionMode.UNSPECIFIED:
      return null;
  }
}
