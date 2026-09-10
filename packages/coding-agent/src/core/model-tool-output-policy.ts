import { createHash } from "node:crypto";
import { lstatSync, mkdirSync, readFileSync, realpathSync } from "node:fs";
import { basename, dirname, relative, resolve } from "node:path";
import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { writeFileAtomicSync } from "../utils/atomic-file.js";

export const MODEL_TOOL_OUTPUT_POLICY_ENV = "PRIME_AGENT_MODEL_TOOL_OUTPUT_POLICY";
export const MODEL_TOOL_OUTPUT_POLICY_OFF = "off";
export const REPEATED_LARGE_TEXT_POLICY = "repeated-large-text-v1";
export const MODEL_TOOL_OUTPUT_MIN_BYTES = 16 * 1024;
const ARTIFACT_DIRECTORY = "model-tool-output-v1";

export type ModelToolOutputPolicy = typeof MODEL_TOOL_OUTPUT_POLICY_OFF | typeof REPEATED_LARGE_TEXT_POLICY;

export interface ModelToolOutputArtifactV1 {
	version: 1;
	artifactId: string;
	sessionId: string;
	filePath: string;
	sha256: string;
	sizeBytes: number;
}

export interface ModelToolOutputScope {
	sessionId: string;
	sessionArtifactDir: string;
}

export interface ModelToolOutputPolicyOptions {
	policy?: ModelToolOutputPolicy;
	scope?: ModelToolOutputScope;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === "object" && !Array.isArray(value);
}

function sha256(text: string): string {
	return createHash("sha256").update(text, "utf8").digest("hex");
}

function artifactDirectory(scope: ModelToolOutputScope): string {
	return resolve(scope.sessionArtifactDir, ARTIFACT_DIRECTORY);
}

function expectedArtifactPath(scope: ModelToolOutputScope, digest: string): string {
	return resolve(artifactDirectory(scope), `${digest}.txt`);
}

function assertSafeScope(scope: ModelToolOutputScope): void {
	if (!scope.sessionId.trim()) throw new Error("Tool-output artifact session id is empty");
	if (!scope.sessionArtifactDir.trim()) throw new Error("Tool-output artifact directory is empty");
}

export function resolveModelToolOutputPolicy(
	value: string | undefined = process.env[MODEL_TOOL_OUTPUT_POLICY_ENV],
): ModelToolOutputPolicy {
	return value === REPEATED_LARGE_TEXT_POLICY ? REPEATED_LARGE_TEXT_POLICY : MODEL_TOOL_OUTPUT_POLICY_OFF;
}

export function isModelToolOutputArtifact(value: unknown): value is ModelToolOutputArtifactV1 {
	return (
		isRecord(value) &&
		value.version === 1 &&
		typeof value.artifactId === "string" &&
		value.artifactId.length > 0 &&
		typeof value.sessionId === "string" &&
		value.sessionId.length > 0 &&
		typeof value.filePath === "string" &&
		value.filePath.length > 0 &&
		typeof value.sha256 === "string" &&
		/^[a-f0-9]{64}$/.test(value.sha256) &&
		typeof value.sizeBytes === "number" &&
		Number.isSafeInteger(value.sizeBytes) &&
		value.sizeBytes >= 0
	);
}

function assertArtifactEnvelope(reference: ModelToolOutputArtifactV1, scope: ModelToolOutputScope): string {
	assertSafeScope(scope);
	if (reference.sessionId !== scope.sessionId) {
		throw new Error("Tool-output artifact belongs to another session");
	}
	if (reference.artifactId !== `tool-output-v1:${reference.sha256}`) {
		throw new Error("Tool-output artifact id does not match its digest");
	}
	const root = artifactDirectory(scope);
	const expected = expectedArtifactPath(scope, reference.sha256);
	if (
		resolve(reference.filePath) !== expected ||
		dirname(expected) !== root ||
		basename(expected) !== `${reference.sha256}.txt`
	) {
		throw new Error("Tool-output artifact path is outside the current session scope");
	}
	const relativePath = relative(root, expected);
	if (!relativePath || relativePath.startsWith("..") || resolve(root, relativePath) !== expected) {
		throw new Error("Tool-output artifact path failed containment validation");
	}
	return expected;
}

function assertSessionArtifactRoot(scope: ModelToolOutputScope): string {
	const sessionRoot = resolve(scope.sessionArtifactDir);
	const sessionStat = lstatSync(sessionRoot);
	if (!sessionStat.isDirectory() || sessionStat.isSymbolicLink()) {
		throw new Error("Tool-output session artifact root is not a regular directory");
	}
	return realpathSync(sessionRoot);
}

function assertArtifactRoot(scope: ModelToolOutputScope): string {
	const sessionReal = assertSessionArtifactRoot(scope);
	const artifactRoot = artifactDirectory(scope);
	const artifactStat = lstatSync(artifactRoot);
	if (!artifactStat.isDirectory() || artifactStat.isSymbolicLink()) {
		throw new Error("Tool-output artifact root is not a regular directory");
	}
	const artifactReal = realpathSync(artifactRoot);
	if (dirname(artifactReal) !== sessionReal) {
		throw new Error("Tool-output artifact root resolved outside the current session");
	}
	return artifactReal;
}

export function readModelToolOutputArtifact(reference: ModelToolOutputArtifactV1, scope: ModelToolOutputScope): string {
	const expected = assertArtifactEnvelope(reference, scope);
	const rootReal = assertArtifactRoot(scope);
	const stat = lstatSync(expected);
	if (!stat.isFile() || stat.isSymbolicLink()) {
		throw new Error("Tool-output artifact is not a regular file");
	}
	if (stat.size !== reference.sizeBytes) {
		throw new Error("Tool-output artifact size check failed");
	}
	const fileReal = realpathSync(expected);
	if (dirname(fileReal) !== rootReal) {
		throw new Error("Tool-output artifact resolved outside the current session scope");
	}
	const bytes = readFileSync(fileReal);
	if (bytes.length !== reference.sizeBytes || createHash("sha256").update(bytes).digest("hex") !== reference.sha256) {
		throw new Error("Tool-output artifact identity check failed");
	}
	return bytes.toString("utf8");
}

export function persistModelToolOutputArtifact(text: string, scope: ModelToolOutputScope): ModelToolOutputArtifactV1 {
	assertSafeScope(scope);
	const bytes = Buffer.from(text, "utf8");
	const digest = createHash("sha256").update(bytes).digest("hex");
	const root = artifactDirectory(scope);
	const filePath = expectedArtifactPath(scope, digest);
	// Validate the session root before creating a child so a symlink/reparse-point
	// scope cannot make even the directory creation escape the session.
	assertSessionArtifactRoot(scope);
	mkdirSync(root, { recursive: true, mode: 0o700 });
	assertArtifactRoot(scope);
	const reference: ModelToolOutputArtifactV1 = {
		version: 1,
		artifactId: `tool-output-v1:${digest}`,
		sessionId: scope.sessionId,
		filePath,
		sha256: digest,
		sizeBytes: bytes.length,
	};
	try {
		const existing = readModelToolOutputArtifact(reference, scope);
		if (existing !== text) throw new Error("Tool-output artifact digest collision");
		return reference;
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
	}
	writeFileAtomicSync(filePath, text, {
		mode: 0o600,
		fsync: true,
		fsyncDir: true,
		beforeRename: (tempPath) => {
			const temp = readFileSync(tempPath);
			if (temp.length !== bytes.length || createHash("sha256").update(temp).digest("hex") !== digest) {
				throw new Error("Tool-output artifact temporary write validation failed");
			}
		},
	});
	if (readModelToolOutputArtifact(reference, scope) !== text) {
		throw new Error("Tool-output artifact verification failed after commit");
	}
	return reference;
}

function toolArtifactFromMessage(message: AgentMessage): ModelToolOutputArtifactV1 | undefined {
	if (message.role !== "toolResult" || !isRecord(message.details)) return undefined;
	return isModelToolOutputArtifact(message.details.modelOutputArtifact)
		? message.details.modelOutputArtifact
		: undefined;
}

interface RepeatedTextCandidate {
	text: string;
	toolCallId: string;
	artifact: ModelToolOutputArtifactV1;
}

function repeatedTextCandidate(message: AgentMessage): RepeatedTextCandidate | undefined {
	if (
		message.role !== "toolResult" ||
		message.toolName !== "ipython" ||
		message.isError ||
		message.content.length !== 1 ||
		message.content[0]?.type !== "text"
	) {
		return undefined;
	}
	const details = isRecord(message.details) ? message.details : undefined;
	if (
		!details ||
		details.status !== "ok" ||
		(typeof details.stderr === "string" && details.stderr.length > 0) ||
		(typeof details.backgroundOutput === "string" && details.backgroundOutput.length > 0) ||
		details.kernelRestarted === true ||
		(Array.isArray(details.diffs) && details.diffs.length > 0) ||
		(Array.isArray(details.attachments) && details.attachments.length > 0) ||
		(Array.isArray(details.sentAgentMessages) && details.sentAgentMessages.length > 0)
	) {
		return undefined;
	}
	const text = message.content[0].text;
	const sizeBytes = Buffer.byteLength(text, "utf8");
	if (sizeBytes < MODEL_TOOL_OUTPUT_MIN_BYTES) return undefined;
	const artifact = toolArtifactFromMessage(message);
	if (!artifact || artifact.sizeBytes !== sizeBytes || artifact.sha256 !== sha256(text)) return undefined;
	return { text, toolCallId: message.toolCallId, artifact };
}

function repeatedOutputNotice(candidate: RepeatedTextCandidate): string {
	const reference = candidate.artifact;
	return `<repeated_tool_output version="1">
The IPython cell executed normally. Its text result is byte-for-byte identical to tool call ${JSON.stringify(candidate.toolCallId)}; no execution was cached or skipped.
The complete result is stored in this session-scoped artifact:
artifact_id: ${reference.artifactId}
session_id: ${JSON.stringify(reference.sessionId)}
path: ${JSON.stringify(reference.filePath)}
sha256: ${reference.sha256}
size_bytes: ${reference.sizeBytes}
To recover any omitted section, use IPython to read the exact path, then verify both SHA-256 and byte size before trusting the content. A missing or mismatched artifact is a retrieval error, not an empty successful result.
</repeated_tool_output>`;
}

/**
 * Reduce only later, byte-identical, large successful IPython text results.
 * Every tool result remains in place with its original call id. The first copy
 * and every error/image/notice remain unchanged. Missing artifacts fail open to
 * the full transcript text instead of producing an unusable reference.
 */
export function applyModelToolOutputPolicy(
	messages: readonly AgentMessage[],
	options: ModelToolOutputPolicyOptions = {},
): AgentMessage[] {
	const policy = options.policy ?? resolveModelToolOutputPolicy();
	const scope = options.scope;
	if (policy !== REPEATED_LARGE_TEXT_POLICY || !scope) return [...messages];
	const seen = new Map<string, RepeatedTextCandidate>();
	return messages.map((message) => {
		const candidate = repeatedTextCandidate(message);
		if (!candidate) return message;
		const key = `${candidate.artifact.sha256}:${candidate.artifact.sizeBytes}`;
		const previous = seen.get(key);
		if (!previous) {
			seen.set(key, candidate);
			return message;
		}
		if (previous.text !== candidate.text) return message;
		try {
			if (readModelToolOutputArtifact(previous.artifact, scope) !== previous.text) return message;
			if (readModelToolOutputArtifact(candidate.artifact, scope) !== candidate.text) return message;
		} catch {
			return message;
		}
		return {
			...message,
			content: [{ type: "text", text: repeatedOutputNotice(previous) }],
		};
	});
}
