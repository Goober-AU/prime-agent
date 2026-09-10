import {
	existsSync,
	mkdirSync,
	mkdtempSync,
	renameSync,
	rmSync,
	symlinkSync,
	unlinkSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { getModel, type Model, type ToolResultMessage } from "@earendil-works/pi-ai";
import { afterEach, describe, expect, it, vi } from "vitest";
import { convertResponsesMessages } from "../../ai/src/providers/openai-responses-shared.js";
import type { KernelClient } from "../src/core/kernel/index.js";
import { convertToLlm } from "../src/core/messages.js";
import {
	applyModelToolOutputPolicy,
	MODEL_TOOL_OUTPUT_MIN_BYTES,
	type ModelToolOutputArtifactV1,
	type ModelToolOutputScope,
	persistModelToolOutputArtifact,
	REPEATED_LARGE_TEXT_POLICY,
	readModelToolOutputArtifact,
} from "../src/core/model-tool-output-policy.js";
import { createIpythonToolDefinition, type IpythonKernelProvisioner } from "../src/core/tools/ipython.js";

const roots: string[] = [];
afterEach(() => {
	while (roots.length > 0) rmSync(roots.pop()!, { recursive: true, force: true });
});

function createScope(sessionId = "session-a"): ModelToolOutputScope {
	const sessionArtifactDir = mkdtempSync(join(tmpdir(), "prime-tool-output-policy-"));
	roots.push(sessionArtifactDir);
	return { sessionId, sessionArtifactDir };
}

function largeText(middle = "OMITTED-MIDDLE-FACT"): string {
	return `header\n${"alpha βeta 0123456789\n".repeat(900)}${middle}\nfooter`;
}

function toolResult(
	toolCallId: string,
	text: string,
	scope: ModelToolOutputScope,
	details: Record<string, unknown> = {},
	content: ToolResultMessage["content"] = [{ type: "text", text }],
	isError = false,
	toolName = "ipython",
): ToolResultMessage {
	const artifact = persistModelToolOutputArtifact(text, scope);
	return {
		role: "toolResult",
		toolCallId,
		toolName,
		content,
		isError,
		details: {
			status: "ok",
			stdout: text,
			stderr: "",
			modelOutputArtifact: artifact,
			...details,
		},
		timestamp: Date.now(),
	};
}

function apply(messages: readonly AgentMessage[], scope: ModelToolOutputScope): AgentMessage[] {
	return applyModelToolOutputPolicy(messages, { policy: REPEATED_LARGE_TEXT_POLICY, scope });
}

function resultText(message: AgentMessage): string {
	if (message.role !== "toolResult" || message.content[0]?.type !== "text") throw new Error("expected text result");
	return message.content[0].text;
}

function responsesModel(): Model<"openai-responses"> {
	const { compat: _compat, ...base } = getModel("openai", "gpt-4o-mini");
	return { ...base, api: "openai-responses" };
}

function pairedTranscript(first: ToolResultMessage, second: ToolResultMessage): AgentMessage[] {
	const model = responsesModel();
	const emptyUsage = {
		input: 0,
		output: 0,
		cacheRead: 0,
		cacheWrite: 0,
		totalTokens: 0,
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
	};
	return [
		{ role: "user", content: "Run both cells", timestamp: 1 },
		{
			role: "assistant",
			content: [{ type: "toolCall", id: first.toolCallId, name: "ipython", arguments: { code: "first()" } }],
			api: model.api,
			provider: model.provider,
			model: model.id,
			usage: emptyUsage,
			stopReason: "toolUse",
			timestamp: 2,
		},
		first,
		{
			role: "assistant",
			content: [{ type: "toolCall", id: second.toolCallId, name: "ipython", arguments: { code: "second()" } }],
			api: model.api,
			provider: model.provider,
			model: model.id,
			usage: emptyUsage,
			stopReason: "toolUse",
			timestamp: 4,
		},
		second,
	];
}

describe("model-facing repeated tool-output policy", () => {
	it("is default-off and leaves converted messages and Responses wire input byte-identical", () => {
		const scope = createScope();
		const text = largeText();
		const transcript = pairedTranscript(toolResult("call-1", text, scope), toolResult("call-2", text, scope));
		const before = convertToLlm(transcript);
		const explicitOff = convertToLlm(transcript, { policy: "off", scope });
		expect(JSON.stringify(explicitOff)).toBe(JSON.stringify(before));
		expect(resultText(explicitOff.at(-1)!)).toBe(text);

		const model = responsesModel();
		const beforeWire = convertResponsesMessages(model, { messages: before }, new Set(["openai"]));
		const offWire = convertResponsesMessages(model, { messages: explicitOff }, new Set(["openai"]));
		expect(JSON.stringify(offWire)).toBe(JSON.stringify(beforeWire));
	});

	it("never caches execution: two identical cells run twice before provider-only reduction", async () => {
		const scope = createScope();
		const text = largeText();
		const execute = vi.fn<KernelClient["execute"]>().mockResolvedValue({
			stdout: text,
			stderr: "",
			status: "ok",
			durationMs: 1,
		});
		const manager = { execute } as unknown as KernelClient;
		const provisioner = {
			modelToolOutputScope: scope,
			modelToolOutputPolicy: REPEATED_LARGE_TEXT_POLICY,
			ensure: vi.fn(async () => manager),
			kill: vi.fn(async () => {}),
		} as unknown as IpythonKernelProvisioner;
		const tool = createIpythonToolDefinition(scope.sessionArtifactDir, {
			provisioner,
			modelToolOutputPolicy: REPEATED_LARGE_TEXT_POLICY,
		});

		const first = await tool.execute(
			"actual-call-1",
			{ code: "expensive()" },
			undefined,
			undefined,
			undefined as never,
		);
		const second = await tool.execute(
			"actual-call-2",
			{ code: "expensive()" },
			undefined,
			undefined,
			undefined as never,
		);
		expect(execute).toHaveBeenCalledTimes(2);
		expect(execute).toHaveBeenNthCalledWith(1, "expensive()", expect.any(Object));
		expect(execute).toHaveBeenNthCalledWith(2, "expensive()", expect.any(Object));
		expect(first.content).toEqual([{ type: "text", text }]);
		expect(second.content).toEqual([{ type: "text", text }]);
		expect(first.details.modelOutputArtifact).toBeDefined();
		expect(second.details.modelOutputArtifact).toBeDefined();

		const providerOnly = apply(
			[
				{ ...first, role: "toolResult", toolName: "ipython", toolCallId: "actual-call-1", timestamp: 1 },
				{ ...second, role: "toolResult", toolName: "ipython", toolCallId: "actual-call-2", timestamp: 2 },
			] as AgentMessage[],
			scope,
		);
		expect(resultText(providerOnly[0])).toBe(text);
		expect(resultText(providerOnly[1])).toContain("no execution was cached or skipped");
	});

	it("executes the same read again and keeps changed same-size file content inline", async () => {
		const scope = createScope();
		const firstText = largeText("FILE-VERSION-A");
		const secondText = largeText("FILE-VERSION-B");
		expect(Buffer.byteLength(firstText)).toBe(Buffer.byteLength(secondText));
		const execute = vi
			.fn<KernelClient["execute"]>()
			.mockResolvedValueOnce({ stdout: firstText, stderr: "", status: "ok", durationMs: 1 })
			.mockResolvedValueOnce({ stdout: secondText, stderr: "", status: "ok", durationMs: 1 });
		const provisioner = {
			modelToolOutputScope: scope,
			modelToolOutputPolicy: REPEATED_LARGE_TEXT_POLICY,
			ensure: vi.fn(async () => ({ execute }) as unknown as KernelClient),
			kill: vi.fn(async () => {}),
		} as unknown as IpythonKernelProvisioner;
		const tool = createIpythonToolDefinition(scope.sessionArtifactDir, { provisioner });

		const first = await tool.execute(
			"read-a",
			{ code: "Path('mutable.txt').read_text()" },
			undefined,
			undefined,
			undefined as never,
		);
		const second = await tool.execute(
			"read-b",
			{ code: "Path('mutable.txt').read_text()" },
			undefined,
			undefined,
			undefined as never,
		);
		expect(execute).toHaveBeenCalledTimes(2);
		const converted = apply(
			[
				{ ...first, role: "toolResult", toolName: "ipython", toolCallId: "read-a", timestamp: 1 },
				{ ...second, role: "toolResult", toolName: "ipython", toolCallId: "read-b", timestamp: 2 },
			] as AgentMessage[],
			scope,
		);
		expect(resultText(converted[0])).toBe(firstText);
		expect(resultText(converted[1])).toBe(secondText);
	});

	it("reduces only later exact copies while preserving every call/result id and order", () => {
		const scope = createScope();
		const text = largeText();
		const transcript = pairedTranscript(toolResult("call-1", text, scope), toolResult("call-2", text, scope));
		const converted = apply(transcript, scope);
		expect(resultText(converted[2])).toBe(text);
		const notice = resultText(converted[4]);
		expect(notice).not.toContain(text.slice(0, 100));
		expect(notice).toContain("The IPython cell executed normally");
		expect(notice).toContain('tool call "call-1"');
		expect(notice).toContain("no execution was cached or skipped");
		expect(notice).toContain("artifact_id: tool-output-v1:");
		expect(notice).toContain(`session_id: ${JSON.stringify(scope.sessionId)}`);
		expect(notice).toContain("sha256:");
		expect(notice).toContain("size_bytes:");
		expect(converted.map((message) => message.role)).toEqual([
			"user",
			"assistant",
			"toolResult",
			"assistant",
			"toolResult",
		]);
		expect((converted[1] as { content: Array<{ id: string }> }).content[0].id).toBe("call-1");
		expect((converted[2] as ToolResultMessage).toolCallId).toBe("call-1");
		expect((converted[3] as { content: Array<{ id: string }> }).content[0].id).toBe("call-2");
		expect((converted[4] as ToolResultMessage).toolCallId).toBe("call-2");
	});

	it("preserves an interleaved subagent notice and both surrounding result IDs", () => {
		const scope = createScope();
		const text = largeText();
		const first = toolResult("subagent-before", text, scope);
		const second = toolResult("subagent-after", text, scope);
		const childNotice: AgentMessage = {
			role: "custom",
			customType: "rlm_child_terminal_notice",
			content: "Child completed between the two executed cells.",
			display: true,
			timestamp: 2,
		};
		const converted = apply([first, childNotice, second], scope);
		expect(converted[0]).toMatchObject({ role: "toolResult", toolCallId: "subagent-before" });
		expect(converted[1]).toEqual(childNotice);
		expect(converted[2]).toMatchObject({ role: "toolResult", toolCallId: "subagent-after" });
		expect(resultText(converted[2])).toContain("<repeated_tool_output");
	});

	it("does not merge changed same-size output and handles exact Unicode bytes", () => {
		const scope = createScope();
		const firstText = `${"é".repeat(MODEL_TOOL_OUTPUT_MIN_BYTES)}A`;
		const changedSameBytes = `${"é".repeat(MODEL_TOOL_OUTPUT_MIN_BYTES)}B`;
		expect(Buffer.byteLength(firstText)).toBe(Buffer.byteLength(changedSameBytes));
		const changed = apply(
			[toolResult("call-a", firstText, scope), toolResult("call-b", changedSameBytes, scope)],
			scope,
		);
		expect(resultText(changed[1])).toBe(changedSameBytes);
		const exact = apply([toolResult("call-c", firstText, scope), toolResult("call-d", firstText, scope)], scope);
		expect(resultText(exact[1])).toContain("<repeated_tool_output");
	});

	it.each([
		["error", { status: "error", error: { traceback: ["secret traceback"] } }, true, "ipython"],
		["stderr", { stderr: "warning" }, false, "ipython"],
		["background", { backgroundOutput: "late output" }, false, "ipython"],
		["kernel restart", { kernelRestarted: true }, false, "ipython"],
		["diff", { diffs: [{ path: "a" }] }, false, "ipython"],
		["sent message", { sentAgentMessages: [{ text: "notice" }] }, false, "ipython"],
		["attachment notice", { attachments: [{ mimeType: "text/plain" }] }, false, "ipython"],
		["other tool", {}, false, "bash"],
	] as const)("keeps %s output fully inline", (_name, details, isError, toolName) => {
		const scope = createScope();
		const text = largeText();
		const converted = apply(
			[
				toolResult("call-a", text, scope, details, undefined, isError, toolName),
				toolResult("call-b", text, scope, details, undefined, isError, toolName),
			],
			scope,
		);
		expect(resultText(converted[1])).toBe(text);
	});

	it("keeps empty, small, and image-bearing results fully inline", () => {
		const scope = createScope();
		const empty = toolResult("empty-a", "", scope);
		const emptyAgain = toolResult("empty-b", "", scope);
		expect(resultText(apply([empty, emptyAgain], scope)[1])).toBe("");

		const small = "x".repeat(MODEL_TOOL_OUTPUT_MIN_BYTES - 1);
		expect(
			resultText(apply([toolResult("small-a", small, scope), toolResult("small-b", small, scope)], scope)[1]),
		).toBe(small);

		const text = largeText();
		const imageContent: ToolResultMessage["content"] = [
			{ type: "text", text },
			{ type: "image", data: "ZmFrZQ==", mimeType: "image/png" },
		];
		const images = apply(
			[
				toolResult("image-a", text, scope, { attachments: [{ mimeType: "image/png" }] }, imageContent),
				toolResult("image-b", text, scope, { attachments: [{ mimeType: "image/png" }] }, imageContent),
			],
			scope,
		);
		expect((images[1] as ToolResultMessage).content).toEqual(imageContent);
	});

	it("fails open to full text for cross-session, traversal, missing, and corrupt references", () => {
		const scope = createScope("session-owner");
		const text = largeText();
		const first = toolResult("call-a", text, scope);
		const second = toolResult("call-b", text, scope);
		const reference = (second.details as { modelOutputArtifact: ModelToolOutputArtifactV1 }).modelOutputArtifact;

		const otherScope = { ...scope, sessionId: "session-other" };
		expect(resultText(apply([first, second], otherScope)[1])).toBe(text);

		const originalPath = reference.filePath;
		reference.filePath = join(scope.sessionArtifactDir, "..", "escape.txt");
		expect(resultText(apply([first, second], scope)[1])).toBe(text);
		reference.filePath = originalPath;

		unlinkSync(reference.filePath);
		expect(resultText(apply([first, second], scope)[1])).toBe(text);
		const repaired = persistModelToolOutputArtifact(text, scope);
		(second.details as { modelOutputArtifact: ModelToolOutputArtifactV1 }).modelOutputArtifact = repaired;
		(first.details as { modelOutputArtifact: ModelToolOutputArtifactV1 }).modelOutputArtifact = repaired;
		writeFileSync(repaired.filePath, "corrupt", "utf8");
		expect(resultText(apply([first, second], scope)[1])).toBe(text);
	});

	it("rejects a reparse-point session root before creating any external artifact directory", () => {
		const holder = createScope();
		const externalRoot = mkdtempSync(join(tmpdir(), "prime-tool-output-external-scope-"));
		roots.push(externalRoot);
		const linkedRoot = join(holder.sessionArtifactDir, "linked-session");
		symlinkSync(externalRoot, linkedRoot, process.platform === "win32" ? "junction" : "dir");
		expect(() =>
			persistModelToolOutputArtifact(largeText(), { sessionId: "linked", sessionArtifactDir: linkedRoot }),
		).toThrow(/session artifact root/i);
		expect(existsSync(join(externalRoot, "model-tool-output-v1"))).toBe(false);
	});

	it("rejects an artifact root replaced by a reparse link", () => {
		const scope = createScope();
		const text = largeText();
		const reference = persistModelToolOutputArtifact(text, scope);
		const artifactRoot = join(scope.sessionArtifactDir, "model-tool-output-v1");
		const movedRoot = join(scope.sessionArtifactDir, "moved-artifacts");
		const externalRoot = mkdtempSync(join(tmpdir(), "prime-tool-output-external-"));
		roots.push(externalRoot);
		renameSync(artifactRoot, movedRoot);
		mkdirSync(externalRoot, { recursive: true });
		symlinkSync(externalRoot, artifactRoot, process.platform === "win32" ? "junction" : "dir");
		expect(() => readModelToolOutputArtifact(reference, scope)).toThrow(/artifact root|session scope/i);
	});

	it("retrieves the omitted middle exactly after recreated reopen, reconnect, and model-switch scopes", () => {
		const scope = createScope();
		const text = largeText("DECISION: retain exact artifact anchor C:/evidence/fact.json");
		const first = toolResult("call-a", text, scope);
		const second = toolResult("call-b", text, scope);
		const reference = (second.details as { modelOutputArtifact: ModelToolOutputArtifactV1 }).modelOutputArtifact;

		const persistedMessages = JSON.parse(JSON.stringify([first, second])) as AgentMessage[];
		for (const lifecycle of [
			{ name: "reopen", messages: persistedMessages, scope: { ...scope } },
			{
				name: "reconnect",
				messages: [first, second],
				scope: { sessionId: `${scope.sessionId}`, sessionArtifactDir: `${scope.sessionArtifactDir}` },
			},
			{ name: "model switch", messages: persistedMessages, scope: { ...scope } },
		]) {
			const converted = convertToLlm(lifecycle.messages, {
				policy: REPEATED_LARGE_TEXT_POLICY,
				scope: lifecycle.scope,
			});
			expect(resultText(converted[1] as AgentMessage), lifecycle.name).toContain("<repeated_tool_output");
			expect(readModelToolOutputArtifact(reference, lifecycle.scope), lifecycle.name).toBe(text);
		}

		const bytes = Buffer.from(readModelToolOutputArtifact(reference, scope), "utf8");
		expect(bytes.length).toBe(reference.sizeBytes);
		expect(text).toContain("DECISION: retain exact artifact anchor C:/evidence/fact.json");
	});

	it("keeps the first surviving copy full after compaction removes its earlier duplicate", () => {
		const scope = createScope();
		const text = largeText();
		const beforeCompaction = apply([toolResult("removed", text, scope), toolResult("retained", text, scope)], scope);
		expect(resultText(beforeCompaction[1])).toContain("<repeated_tool_output");
		const survivingTranscript = [toolResult("retained", text, scope)];
		expect(resultText(apply(survivingTranscript, scope)[0])).toBe(text);
	});

	it("changes only the later function output in real Responses serialization", () => {
		const scope = createScope();
		const text = largeText();
		const transcript = pairedTranscript(toolResult("call-1", text, scope), toolResult("call-2", text, scope));
		const model = responsesModel();
		const off = convertResponsesMessages(model, { messages: convertToLlm(transcript) }, new Set(["openai"]));
		const on = convertResponsesMessages(
			model,
			{ messages: convertToLlm(transcript, { policy: REPEATED_LARGE_TEXT_POLICY, scope }) },
			new Set(["openai"]),
		);
		const offOutputs = off.filter((item) => item.type === "function_call_output");
		const onOutputs = on.filter((item) => item.type === "function_call_output");
		expect(onOutputs).toHaveLength(2);
		expect(onOutputs[0]).toEqual(offOutputs[0]);
		expect(onOutputs[1]?.call_id).toBe(offOutputs[1]?.call_id);
		expect(onOutputs[1]?.output).toContain("<repeated_tool_output");
		expect(JSON.stringify(onOutputs[1])).toContain("no execution was cached or skipped");
	});
});
