import { createHash } from "node:crypto";
import type { AgentMessage } from "@earendil-works/pi-agent-core";

export interface MemorySource {
	id: string;
	origin: "user" | "assistant" | "tool" | "derived" | "file";
	sha256: string;
	uri?: string;
	revision?: string;
	projectPath?: string;
}
export interface Evidence extends MemorySource {
	text: string;
}
export const MEMORY_RECALL_TYPE = "prime-agent.memory-recall";
export function hash(value: string): string {
	return createHash("sha256").update(value).digest("hex");
}
export function messageEvidence(message: AgentMessage, uri?: string, entryId?: string): Evidence | undefined {
	// Custom messages include recalled memory, refinement notices and other host injections.
	if (message.role === "custom") return undefined;
	let origin: MemorySource["origin"];
	let text: string;
	if (message.role === "user" || message.role === "assistant" || message.role === "toolResult") {
		origin = message.role === "toolResult" ? "tool" : message.role;
		text =
			typeof message.content === "string"
				? message.content
				: message.content
						.map((block) => {
							if (block.type === "text") return block.text;
							if (block.type === "toolCall")
								return `[tool call ${block.name}] ${JSON.stringify(block.arguments)}`;
							return `[${block.type} omitted]`;
						})
						.join("\n");
		if (message.role === "toolResult")
			text = `[${message.toolName}; call=${message.toolCallId}; error=${message.isError}]\n${text}`;
	} else if (message.role === "bashExecution") {
		if (message.excludeFromContext) return undefined;
		origin = "tool";
		text = `[bash ${message.command}; exit=${message.exitCode}]\n${message.output}`;
	} else if (message.role === "compactionSummary" || message.role === "branchSummary") {
		origin = "derived";
		text = message.summary;
	} else return undefined;
	if (origin === "tool" && text.includes("[memory data; not new evidence]")) origin = "derived";
	const sha256 = hash(text);
	return {
		id: entryId ?? `msg_${hash(`${message.role}:${message.timestamp}:${sha256}`).slice(0, 24)}`,
		origin,
		text,
		sha256,
		uri,
	};
}
export function collectEvidence(messages: readonly AgentMessage[]): Evidence[] {
	return messages.flatMap((message) => {
		const evidence = messageEvidence(message);
		return evidence ? [evidence] : [];
	});
}
export function evidenceWindow(records: readonly Evidence[], maxChars = 80_000): { text: string; ids: string[] } {
	const ids: string[] = [];
	const output: string[] = [];
	let remaining = maxChars;
	for (const record of [...records].reverse()) {
		const label = JSON.stringify({ id: record.id, origin: record.origin, sha256: record.sha256, uri: record.uri });
		const prefix = `[Evidence ${label}]\n`;
		if (remaining <= prefix.length + 50) break;
		const text =
			record.text.length + prefix.length <= remaining
				? record.text
				: `${record.text.slice(0, remaining - prefix.length - 30)}\n[truncated; inspect source]`;
		const block = `${prefix}${text}`;
		output.unshift(block);
		ids.unshift(record.id);
		remaining -= block.length + 2;
	}
	return { text: output.join("\n\n"), ids };
}

export function serializeEvidence(records: readonly Evidence[], maxChars = 80_000): string {
	return evidenceWindow(records, maxChars).text;
}
