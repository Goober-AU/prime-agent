import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { writeFileAtomicSync } from "../src/utils/atomic-file.js";

const io = vi.hoisted(() => ({
	fileSyncError: undefined as NodeJS.ErrnoException | undefined,
	dirSyncError: undefined as NodeJS.ErrnoException | undefined,
	dirOpenError: undefined as NodeJS.ErrnoException | undefined,
	renameError: undefined as NodeJS.ErrnoException | undefined,
	events: [] as string[],
}));

vi.mock("node:fs", () => ({
	openSync: (_path: string, mode: string) => {
		if (mode === "r" && io.dirOpenError) throw io.dirOpenError;
		io.events.push(mode === "r" ? "open-directory" : "open-file");
		return mode === "r" ? 2 : 1;
	},
	writeSync: (_fd: number, _buffer: Buffer, _offset: number, length: number) => {
		io.events.push("write");
		return length;
	},
	fsyncSync: (fd: number) => {
		io.events.push(fd === 1 ? "sync-file" : "sync-directory");
		const error = fd === 1 ? io.fileSyncError : io.dirSyncError;
		if (error) throw error;
	},
	closeSync: (fd: number) => io.events.push(`close-${fd}`),
	renameSync: () => {
		io.events.push("rename");
		if (io.renameError) throw io.renameError;
	},
	rmSync: () => {},
	chmodSync: () => {},
	readlinkSync: vi.fn(),
	realpathSync: vi.fn(),
}));

function failure(code: string, syscall = "fsync"): NodeJS.ErrnoException {
	return Object.assign(new Error(`${code} ${syscall}`), { code, syscall });
}
const write = () =>
	writeFileAtomicSync("/isolated/journal.jsonl", '{"type":"received"}\n', { fsync: true, fsyncDir: true });

describe("atomic directory fsync on Windows", () => {
	beforeEach(() => {
		io.events.length = 0;
		io.fileSyncError = io.dirSyncError = io.dirOpenError = io.renameError = undefined;
		vi.spyOn(process, "platform", "get").mockReturnValue("win32");
	});
	afterEach(() => vi.restoreAllMocks());
	it("only tolerates unsupported directory fsync after file sync and rename", () => {
		io.dirSyncError = failure("EPERM");
		expect(write).not.toThrow();
		expect(io.events).toEqual([
			"open-file",
			"write",
			"sync-file",
			"close-1",
			"rename",
			"open-directory",
			"sync-directory",
			"close-2",
		]);
	});
	it.each(["EIO", "EBADF", "ENOSPC", "EACCES"])("does not suppress unexpected directory fsync %s", (code) => {
		io.dirSyncError = failure(code);
		expect(write).toThrow(`${code} fsync`);
	});
	it("does not tolerate EPERM from directory open", () => {
		io.dirOpenError = failure("EPERM", "open");
		expect(write).toThrow("EPERM open");
	});
	it("never renames after failed journal-file fsync", () => {
		io.fileSyncError = failure("EPERM");
		expect(write).toThrow("EPERM fsync");
		expect(io.events).not.toContain("rename");
	});
	it("does not suppress rename errors", () => {
		io.renameError = failure("EIO", "rename");
		expect(write).toThrow("EIO rename");
		expect(io.events).not.toContain("open-directory");
	});
	it("still fails directory EPERM on POSIX", () => {
		vi.spyOn(process, "platform", "get").mockReturnValue("linux");
		io.dirSyncError = failure("EPERM");
		expect(write).toThrow("EPERM fsync");
	});
});
