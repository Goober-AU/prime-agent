import { randomUUID } from "node:crypto";
import {
	chmodSync,
	closeSync,
	fsyncSync,
	openSync,
	readlinkSync,
	realpathSync,
	renameSync,
	rmSync,
	writeSync,
} from "node:fs";
import { chmod, open, rename, rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";

const WIN32_RENAME_ATTEMPTS = 5;

function sleepSync(ms: number): void {
	Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function isTransientWindowsRenameError(error: unknown, platform: NodeJS.Platform): boolean {
	const code = (error as NodeJS.ErrnoException).code;
	return platform === "win32" && (code === "EPERM" || code === "EACCES" || code === "EBUSY");
}

// Windows raises transient EPERM/EACCES when the destination is held open (antivirus, indexer).
function renameOntoSync(from: string, to: string): void {
	for (let attempt = 1; ; attempt++) {
		try {
			renameSync(from, to);
			return;
		} catch (error) {
			if (!isTransientWindowsRenameError(error, process.platform) || attempt >= WIN32_RENAME_ATTEMPTS) {
				throw error;
			}
			sleepSync(10 * attempt);
		}
	}
}

export interface AtomicRenameRetryEvent {
	attempt: number;
	delayMs: number;
	error: NodeJS.ErrnoException;
}

export interface AtomicRenameRetryOptions {
	attempts?: number;
	platform?: NodeJS.Platform;
	sleep?: (delayMs: number) => Promise<void>;
	/** Best-effort observer. Observer failures never fail the durable write. */
	onRetry?: (event: AtomicRenameRetryEvent) => void;
}

function asyncDelay(delayMs: number): Promise<void> {
	return new Promise((resolveDelay) => setTimeout(resolveDelay, delayMs));
}

async function renameOnto(from: string, to: string, options: AtomicRenameRetryOptions): Promise<void> {
	const attempts = Math.max(1, options.attempts ?? WIN32_RENAME_ATTEMPTS);
	const platform = options.platform ?? process.platform;
	for (let attempt = 1; ; attempt++) {
		try {
			await rename(from, to);
			return;
		} catch (error) {
			if (!isTransientWindowsRenameError(error, platform) || attempt >= attempts) {
				throw error;
			}
			const delayMs = 10 * attempt;
			try {
				options.onRetry?.({ attempt, delayMs, error: error as NodeJS.ErrnoException });
			} catch {
				// Retry observation is disposable; persistence is not.
			}
			await (options.sleep ?? asyncDelay)(delayMs);
		}
	}
}

export interface WriteFileAtomicOptions {
	mode?: number;
	/** fsync the temp file before the rename. */
	fsync?: boolean;
	/** Directory fsync after the rename; tolerates only unsupported Windows directory fsync. */
	fsyncDir?: boolean;
	/** Runs on the written temp file before it replaces the destination (validation, ownership). */
	beforeRename?: (tempPath: string) => void;
}

export interface WriteFileAtomicAsyncOptions extends Omit<WriteFileAtomicOptions, "beforeRename"> {
	beforeRename?: (tempPath: string) => void | Promise<void>;
	renameRetry?: AtomicRenameRetryOptions;
}

function shouldIgnoreDirectoryFsyncError(error: unknown, platform: NodeJS.Platform): boolean {
	const failure = error as NodeJS.ErrnoException;
	return platform === "win32" && failure?.code === "EPERM" && failure?.syscall === "fsync";
}

async function fsyncDirectory(path: string, platform: NodeJS.Platform): Promise<void> {
	let directoryHandle: Awaited<ReturnType<typeof open>> | undefined;
	try {
		directoryHandle = await open(path, "r");
		await directoryHandle.sync();
	} catch (error) {
		if (!shouldIgnoreDirectoryFsyncError(error, platform)) throw error;
	} finally {
		await directoryHandle?.close();
	}
}

/** Durable-write owner: temp file beside the destination, then an atomic rename. */
export function writeFileAtomicSync(path: string, data: string, options: WriteFileAtomicOptions = {}): void {
	const tempPath = `${path}.${process.pid}.${randomUUID()}.tmp`;
	try {
		const descriptor = options.mode === undefined ? openSync(tempPath, "wx") : openSync(tempPath, "wx", options.mode);
		try {
			// writeSync may return a short count without throwing; a partial temp must never be renamed in.
			const bytes = Buffer.from(data, "utf8");
			let offset = 0;
			while (offset < bytes.length) {
				const written = writeSync(descriptor, bytes, offset, bytes.length - offset);
				if (written <= 0) throw new Error(`Short write persisting ${path}`);
				offset += written;
			}
			if (options.fsync) fsyncSync(descriptor);
		} finally {
			closeSync(descriptor);
		}
		// openSync's mode is masked by the umask; enforce the requested bits exactly.
		if (options.mode !== undefined) chmodSync(tempPath, options.mode);
		options.beforeRename?.(tempPath);
		renameOntoSync(tempPath, path);
	} finally {
		rmSync(tempPath, { force: true });
	}
	if (options.fsyncDir) {
		try {
			const directoryDescriptor = openSync(dirname(path), "r");
			try {
				fsyncSync(directoryDescriptor);
			} finally {
				closeSync(directoryDescriptor);
			}
		} catch (error) {
			if (!shouldIgnoreDirectoryFsyncError(error, process.platform)) {
				throw error;
			}
		}
	}
}

/** Async atomic replacement for daemon-owned writes that must not block the event loop during rename contention. */
export async function writeFileAtomic(
	path: string,
	data: string,
	options: WriteFileAtomicAsyncOptions = {},
): Promise<void> {
	const tempPath = `${path}.${process.pid}.${randomUUID()}.tmp`;
	try {
		const handle = await open(tempPath, "wx", options.mode);
		try {
			const bytes = Buffer.from(data, "utf8");
			let offset = 0;
			while (offset < bytes.length) {
				const { bytesWritten } = await handle.write(bytes, offset, bytes.length - offset);
				if (bytesWritten <= 0) throw new Error(`Short write persisting ${path}`);
				offset += bytesWritten;
			}
			if (options.fsync) await handle.sync();
		} finally {
			await handle.close();
		}
		if (options.mode !== undefined) await chmod(tempPath, options.mode);
		await options.beforeRename?.(tempPath);
		await renameOnto(tempPath, path, options.renameRetry ?? {});
	} finally {
		await rm(tempPath, { force: true });
	}
	if (options.fsyncDir) {
		await fsyncDirectory(dirname(path), options.renameRetry?.platform ?? process.platform);
	}
}

export interface RemoveFileDurablyOptions {
	fsyncDir?: boolean;
	platform?: NodeJS.Platform;
}

/** Remove a lifecycle file and, when requested, durably persist the directory entry change. */
export async function removeFileDurably(path: string, options: RemoveFileDurablyOptions = {}): Promise<void> {
	await rm(path, { force: true });
	if (options.fsyncDir) await fsyncDirectory(dirname(path), options.platform ?? process.platform);
}

export interface AtomicFileWriteResult {
	generation: number;
}

interface AtomicFileTargetState {
	nextGeneration: number;
	tail: Promise<void>;
}

/**
 * Serializes atomic operations per target. Different targets proceed concurrently.
 * Every write gets a monotonic generation and no lifecycle-critical write is coalesced.
 */
export class AtomicFileWriteCoordinator {
	private readonly targets = new Map<string, AtomicFileTargetState>();

	constructor(private readonly platform: NodeJS.Platform = process.platform) {}

	private targetKey(path: string): string {
		const absolute = resolve(path);
		// Windows descriptor paths live inside a trusted lease directory, where lexical case
		// aliases name the same file. Symlink, hard-link, and 8.3 aliases are deliberately
		// outside that contract; callers must pass the lease-owned path rather than an alias.
		return this.platform === "win32" ? absolute.toLowerCase() : absolute;
	}

	write(path: string, data: string, options: WriteFileAtomicAsyncOptions = {}): Promise<AtomicFileWriteResult> {
		return this.run(path, () => writeFileAtomic(path, data, options));
	}

	run(path: string, operation: () => void | Promise<void>): Promise<AtomicFileWriteResult> {
		const target = this.targetKey(path);
		const state = this.targets.get(target) ?? { nextGeneration: 0, tail: Promise.resolve() };
		this.targets.set(target, state);
		const generation = ++state.nextGeneration;
		const execution = state.tail.then(operation, operation);
		state.tail = execution.then(
			() => undefined,
			() => undefined,
		);
		return execution.then(() => ({ generation }));
	}

	async drain(timeoutMs: number): Promise<void> {
		const startedAt = Date.now();
		while (this.targets.size > 0) {
			const pending = new Map([...this.targets].map(([target, state]) => [target, state.tail]));
			const elapsed = Date.now() - startedAt;
			const remaining = Math.max(0, timeoutMs - elapsed);
			let timeout: ReturnType<typeof setTimeout> | undefined;
			try {
				await Promise.race([
					Promise.all(pending.values()),
					new Promise<never>((_resolve, reject) => {
						timeout = setTimeout(
							() => reject(new Error(`Timed out after ${timeoutMs}ms draining atomic file writes`)),
							remaining,
						);
					}),
				]);
			} finally {
				if (timeout) clearTimeout(timeout);
			}
			// A write may have joined an existing target (or created a new one) while
			// this drain was waiting. Observe tails again so shutdown cannot report a
			// successful drain while a late, already-admitted durable write is pending.
			if ([...this.targets].every(([target, state]) => pending.get(target) === state.tail)) return;
		}
	}
}

/** Resolve symlink aliases so a replace lands on the real file (in-place-write parity). */
export function realpathIfPresentSync(path: string): string {
	try {
		return realpathSync(path);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
			throw error;
		}
	}
	// ENOENT also means a DANGLING symlink chain: follow it like in-place writes did.
	let current = path;
	for (let hop = 0; hop < 32; hop++) {
		let target: string;
		try {
			target = readlinkSync(current);
		} catch {
			return current;
		}
		// A relative target resolves against the link's PHYSICAL parent directory.
		let parent = dirname(current);
		try {
			parent = realpathSync(parent);
		} catch {
			// Fall back to the alias parent.
		}
		current = resolve(parent, target);
	}
	// A loud failure beats silently replacing an intermediate link (or looping on a cycle).
	throw new Error(`Too many symlink hops resolving ${path}`);
}
