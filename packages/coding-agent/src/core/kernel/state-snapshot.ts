// Locations and result shapes for the kernel's persisted user namespace, which
// is revived when a session resumes. The kernel is otherwise spawned fresh on
// resume, leaving the model believing it still has access to variables/imports
// it defined earlier.
//
// Snapshotting is best-effort and per-variable: each top-level name is pickled
// with `dill` independently, so a single unpicklable object (open file, socket,
// GPU tensor, …) is skipped and reported rather than aborting the whole snapshot.
import { lstatSync } from "node:fs";
import { join } from "node:path";

/** Default ceiling on a snapshot payload. Over-cap variables are skipped + reported. */
export const DEFAULT_SNAPSHOT_MAX_BYTES = 256 * 1024 * 1024;
/** Default ceiling for one serialized variable. */
export const DEFAULT_SNAPSHOT_MAX_VARIABLE_BYTES = 16 * 1024 * 1024;

/** Base filename for the kernel snapshot within a session's artifact directory. */
const KERNEL_STATE_BASENAME = "kernel-state";

export type KernelSnapshotFormat = "legacy" | "cas-v2";
export type KernelRestoreSource = "auto" | "current" | "previous" | "legacy";

/** Numeric-only timings and byte counts returned by the Python serializer. */
export interface SnapshotPerformanceMetadata {
	serialization_wall_ms: number | null;
	serialization_cpu_ms: number | null;
	serialized_bytes: number | null;
	write_ms: number | null;
	written_bytes: number | null;
	total_wall_ms: number | null;
}

export interface SnapshotResult {
	/** Top-level names successfully serialized into the payload. */
	saved: string[];
	/** Names that could not be serialized, with a short reason. */
	skipped: { name: string; reason: string }[];
	/** Oversized live variables removed by an explicit compaction snapshot. */
	pruned?: string[];
	/** Legacy-equivalent payload bytes, including outer-container overhead. */
	bytes: number;
	/** Sum of retained independent per-name dill blobs. */
	logicalBytes: number;
	/** Bytes physically written by this snapshot attempt, including CAS metadata. */
	writtenBytes: number;
	format: KernelSnapshotFormat;
	generation?: string;
	/** False for CAS v2 until an explicit legacy export is completed. */
	backwardReadable: boolean;
	metrics?: SnapshotPerformanceMetadata;
	path: string;
}

export interface RestoreResult {
	/** Names successfully revived into the kernel namespace. */
	restored: string[];
	/** Names present in the snapshot that failed to revive, with a short reason. */
	failed: { name: string; reason: string }[];
	format?: KernelSnapshotFormat;
	generation?: string;
	/** Previous-generation restore was explicitly requested. */
	rolledBack?: boolean;
	/** The selected source may omit newer committed or unsaved work. */
	unsavedWorkPossible?: boolean;
	/** Legacy was explicitly selected despite a v2 state root. */
	legacyRecovery?: boolean;
	path: string;
}

export interface SnapshotLegacyExportResult {
	exported: string[];
	bytes: number;
	sourceGeneration: string;
	source: "current" | "previous";
	backwardReadable: true;
	path: string;
}

/** Absolute path to the legacy dill payload within a session's artifact directory. */
export function snapshotPathIn(artifactDir: string): string {
	return join(artifactDir, `${KERNEL_STATE_BASENAME}.dill`);
}

/** Absolute path to the legacy JSON manifest within a session's artifact directory. */
export function manifestPathIn(artifactDir: string): string {
	return join(artifactDir, `${KERNEL_STATE_BASENAME}.json`);
}

/** Session-scoped root for CAS v2 blobs, generations, marker, and pointer. */
export function casSnapshotRootIn(artifactDir: string): string {
	return join(artifactDir, `${KERNEL_STATE_BASENAME}.v2`);
}

/** Derive a sibling CAS root for direct manager users that only supply a legacy path. */
export function casSnapshotRootForLegacyPath(snapshotPath: string): string {
	return snapshotPath.endsWith(".dill") ? `${snapshotPath.slice(0, -".dill".length)}.v2` : `${snapshotPath}.v2`;
}

function pathEntryExists(path: string): boolean {
	try {
		return lstatSync(path, { throwIfNoEntry: false }) !== undefined;
	} catch {
		// Permission and malformed-reparse failures are state, not proof of absence.
		return true;
	}
}

/** True for a legacy payload or any v2 root entry, including broken reparse links. */
export function snapshotStateExistsIn(artifactDir: string): boolean {
	return pathEntryExists(snapshotPathIn(artifactDir)) || pathEntryExists(casSnapshotRootIn(artifactDir));
}

/** Manager-level v2 detection when only explicit paths are available. */
export function casSnapshotStateExists(root: string): boolean {
	return pathEntryExists(root);
}
