import { mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { afterEach, describe, expect, it } from "vitest";
import { createAgentObserveMessagePreview } from "../../src/core/agent-observe.js";
import { convertToLlm } from "../../src/core/messages.js";
import {
	buildSessionContext,
	loadEntriesFromFile,
	parseSessionEntries,
	rehydrateSessionFileEntry,
	SessionManager,
	type SessionMessageEntry,
	serializeSessionFileEntry,
} from "../../src/core/session-manager.js";

const tempDirs: string[] = [];

afterEach(() => {
	for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

function tempDir(): string {
	const dir = mkdtempSync(join(tmpdir(), "prime-tool-text-dedup-"));
	tempDirs.push(dir);
	return dir;
}

function toolEntry(overrides: Partial<Record<string, unknown>> = {}): SessionMessageEntry {
	const stdout = `stdout-λ-${"A".repeat(4096)}`;
	const stderr = `stderr-雪-${"B".repeat(4096)}`;
	const result = `result-🙂-${"C".repeat(4096)}`;
	const backgroundOutput = `background-${"D".repeat(4096)}`;
	return {
		type: "message",
		id: "tool-entry",
		parentId: null,
		timestamp: "2026-09-10T00:00:00.000Z",
		message: {
			role: "toolResult",
			toolCallId: "tool-call",
			toolName: "ipython",
			content: [
				{
					type: "text",
					text: `begin\n${stdout}\nbetween\n${stderr}\n${result}\n${backgroundOutput}\nend`,
				},
				{ type: "image", data: "iVBORw0KGgo=", mimeType: "image/png" },
			],
			isError: false,
			timestamp: 42,
			details: {
				stdout,
				stderr,
				result,
				backgroundOutput,
				durationMs: 17,
				truncated: { originalChars: 99_999, keptChars: 16_384 },
				error: { ename: "Example", traceback: ["unique stack line"] },
				...overrides,
			},
		} as unknown as AgentMessage,
	};
}

describe("inline tool-result text deduplication", () => {
	it("reads old rows unchanged and rehydrates the new versioned representation exactly", () => {
		const original = toolEntry();
		const oldRoundTrip = parseSessionEntries(`${JSON.stringify(original)}\n`)[0];
		const encoded = serializeSessionFileEntry(original);
		const disk = JSON.parse(encoded) as SessionMessageEntry;
		const diskDetails = (disk.message as Extract<AgentMessage, { role: "toolResult" }>).details as Record<
			string,
			unknown
		>;

		expect(disk).toMatchObject({
			$primeSessionEncoding: {
				version: 1,
				kind: "inline_tool_text",
				fields: ["stdout", "stderr", "result", "backgroundOutput"],
			},
		});
		expect(diskDetails.stdout).toMatchObject({ $primeToolText: { version: 1, source: "content" } });
		expect(diskDetails.stderr).toMatchObject({ $primeToolText: { version: 1, source: "content" } });
		expect(diskDetails.result).toMatchObject({ $primeToolText: { version: 1, source: "content" } });
		expect(diskDetails.backgroundOutput).toMatchObject({
			$primeToolText: { version: 1, source: "content" },
		});
		expect(Buffer.byteLength(encoded, "utf8")).toBeLessThan(
			Buffer.byteLength(JSON.stringify(original), "utf8") * 0.6,
		);

		const newRoundTrip = parseSessionEntries(`${encoded}\n`)[0];
		expect(oldRoundTrip).toEqual(original);
		expect(newRoundTrip).toEqual(original);
		expect(newRoundTrip).not.toHaveProperty("$primeSessionEncoding");
		expect(rehydrateSessionFileEntry(disk)).toEqual(original);
		expect(createAgentObserveMessagePreview((newRoundTrip as SessionMessageEntry).message, 0, 20_000)).toEqual(
			createAgentObserveMessagePreview(original.message, 0, 20_000),
		);
	});

	it("leaves legacy arbitrary detail objects with marker-like keys unchanged", () => {
		const markerCollision = {
			$primeToolText: {
				version: 1,
				source: "content",
				contentIndex: 0,
				start: 0,
				length: 1,
				sha256: "not-storage-metadata",
			},
			custom: "tool-owned",
		};
		const original = Object.assign(toolEntry({ stdout: markerCollision }), {
			$primeSessionEncoding: { version: 0, kind: "tool-owned", fields: ["stdout"] },
		});
		expect(
			parseSessionEntries(`${JSON.stringify(original)}
`)[0],
		).toEqual(original);
	});

	it("keeps the original row when references plus their disk envelope are not byte-smaller", () => {
		const value = "x".repeat(200);
		const original = toolEntry({ stdout: value, stderr: "unique", result: "unique", backgroundOutput: "unique" });
		const message = original.message as Extract<AgentMessage, { role: "toolResult" }>;
		message.content = [{ type: "text", text: value }];
		expect(serializeSessionFileEntry(original)).toBe(JSON.stringify(original));
	});

	it("preserves empty and unique channels, errors, timing, truncation, images and Unicode", () => {
		const original = toolEntry({
			stdout: "",
			stderr: `not-present-${"unique".repeat(1024)}`,
			backgroundOutput: "",
		});
		const encoded = serializeSessionFileEntry(original);
		const disk = JSON.parse(encoded) as SessionMessageEntry;
		const diskDetails = (disk.message as Extract<AgentMessage, { role: "toolResult" }>).details as Record<
			string,
			unknown
		>;
		expect(diskDetails.stdout).toBe("");
		expect(diskDetails.stderr).toBe(`not-present-${"unique".repeat(1024)}`);
		expect(diskDetails.backgroundOutput).toBe("");
		expect(parseSessionEntries(encoded)[0]).toEqual(original);
	});

	it("keeps public context and provider input byte-for-byte equivalent across persist, compaction and reopen", () => {
		const root = tempDir();
		const sessions = join(root, "sessions");
		mkdirSync(sessions, { recursive: true });
		const manager = SessionManager.create(root, sessions);
		manager.flushNow();
		const original = toolEntry().message;
		const toolId = manager.appendMessage(original as never);
		manager.appendCompaction("compact summary", toolId, 1234);
		const before = manager.buildSessionContext();
		const beforeProvider = JSON.stringify(convertToLlm(before.messages));
		const file = manager.getSessionFile()!;
		const raw = readFileSync(file, "utf8");
		expect(raw).toContain('"$primeToolText"');

		const reopened = SessionManager.open(file, sessions);
		const after = reopened.buildSessionContext();
		expect(after).toEqual(before);
		expect(JSON.stringify(convertToLlm(after.messages))).toBe(beforeProvider);
		expect(loadEntriesFromFile(file)).toEqual([reopened.getHeader(), ...reopened.getEntries()]);
	});

	it("turns an invalid inline reference into a visible tool error rather than empty success", () => {
		const disk = JSON.parse(serializeSessionFileEntry(toolEntry())) as SessionMessageEntry;
		const details = (disk.message as Extract<AgentMessage, { role: "toolResult" }>).details as Record<
			string,
			{ $primeToolText?: { sha256?: string } }
		>;
		details.stdout.$primeToolText!.sha256 = "0".repeat(64);
		const loaded = parseSessionEntries(JSON.stringify(disk))[0] as SessionMessageEntry;
		const message = loaded.message as Extract<AgentMessage, { role: "toolResult" }>;
		expect(loaded).not.toHaveProperty("$primeSessionEncoding");
		expect(message.isError).toBe(true);
		expect(message.details).toMatchObject({ stdout: expect.stringContaining("session recovery error") });
		expect(message.content).toContainEqual({
			type: "text",
			text: expect.stringContaining("could not reconstruct stdout"),
		});
	});

	it("round-trips lone surrogates losslessly and detects replacement-character corruption", () => {
		const loneSurrogates = "\ud800".repeat(2_048);
		const original = toolEntry({
			stdout: loneSurrogates,
			stderr: "unique",
			result: "unique",
			backgroundOutput: "unique",
		});
		const message = original.message as Extract<AgentMessage, { role: "toolResult" }>;
		message.content = [{ type: "text", text: loneSurrogates }];
		const encoded = serializeSessionFileEntry(original);
		expect(parseSessionEntries(encoded)[0]).toEqual(original);

		const corrupted = JSON.parse(encoded) as SessionMessageEntry;
		const corruptedMessage = corrupted.message as Extract<AgentMessage, { role: "toolResult" }>;
		corruptedMessage.content = [{ type: "text", text: "�".repeat(2_048) }];
		const loaded = parseSessionEntries(JSON.stringify(corrupted))[0] as SessionMessageEntry;
		expect((loaded.message as Extract<AgentMessage, { role: "toolResult" }>).isError).toBe(true);
		expect(loaded.message).not.toEqual(original.message);
	});

	it("keeps branch context construction identical for old and new rows", () => {
		const original = toolEntry();
		const oldEntry = parseSessionEntries(JSON.stringify(original))[0] as SessionMessageEntry;
		const newEntry = parseSessionEntries(serializeSessionFileEntry(original))[0] as SessionMessageEntry;
		expect(buildSessionContext([newEntry], newEntry.id)).toEqual(buildSessionContext([oldEntry], oldEntry.id));
	});
});
