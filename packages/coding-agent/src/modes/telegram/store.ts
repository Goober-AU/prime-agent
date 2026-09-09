import { createHash, randomBytes } from "node:crypto";
import { chmodSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { writeFileAtomicSync } from "../../utils/atomic-file.js";

export interface TelegramConnectionSettings {
	version: 1;
	enabled: boolean;
	botToken: string;
	botId: number;
	botUsername: string;
	daemonSocket: string;
	cwd: string;
	sessionId: string;
	sessionFile?: string;
	pairedUserId?: number;
	pairing?: { hash: string; expiresAt: number };
}

export interface TelegramDelivery {
	id: string;
	chatId: number;
	text: string;
}

export interface TelegramState {
	botId: number;
	offset: number;
	outbox: TelegramDelivery[];
	inbox: Array<{ id: number; text: string }>;
	lastAssistantKey?: string;
	interruptedUpdate?: number;
}

export interface TelegramWorkerStatus {
	instanceId: string;
	pid: number;
	updatedAt: number;
	phase: "starting" | "pairing" | "running" | "stopped" | "error";
	error?: string;
}

export function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function isTelegramId(value: unknown): value is number {
	return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}

export function isTelegramUpdateId(value: unknown): value is number {
	return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && value < Number.MAX_SAFE_INTEGER;
}

export function validBotToken(token: string): boolean {
	return /^\d{5,20}:[A-Za-z0-9_-]{20,200}$/.test(token);
}

export function pairingHash(code: string): string {
	return createHash("sha256").update(code).digest("hex");
}

export function createPairing(now = Date.now()): {
	code: string;
	pairing: NonNullable<TelegramConnectionSettings["pairing"]>;
} {
	const code = randomBytes(24).toString("base64url");
	return { code, pairing: { hash: pairingHash(code), expiresAt: now + 10 * 60_000 } };
}

export class TelegramStore {
	readonly directory: string;

	constructor(readonly agentDir: string) {
		this.directory = join(agentDir, "telegram");
	}

	path(name: string): string {
		return join(this.directory, name);
	}

	read(name: string): unknown {
		try {
			return JSON.parse(readFileSync(this.path(name), "utf8"));
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
			throw new Error(`Cannot read Telegram ${name}. Restore or remove that file before reconnecting.`);
		}
	}

	write(name: string, value: unknown): void {
		mkdirSync(this.directory, { recursive: true, mode: 0o700 });
		chmodSync(this.directory, 0o700);
		writeFileAtomicSync(this.path(name), `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600, fsync: true });
	}

	settings(): TelegramConnectionSettings | undefined {
		const value = this.read("connection.json");
		if (value === undefined) return undefined;
		if (
			!isRecord(value) ||
			value.version !== 1 ||
			typeof value.enabled !== "boolean" ||
			typeof value.botToken !== "string" ||
			!validBotToken(value.botToken) ||
			!isTelegramId(value.botId) ||
			typeof value.botUsername !== "string" ||
			!/^[A-Za-z0-9_]{1,64}$/.test(value.botUsername) ||
			typeof value.daemonSocket !== "string" ||
			!value.daemonSocket ||
			typeof value.cwd !== "string" ||
			typeof value.sessionId !== "string" ||
			!value.sessionId ||
			(value.sessionFile !== undefined && typeof value.sessionFile !== "string") ||
			(value.pairedUserId !== undefined && !isTelegramId(value.pairedUserId)) ||
			(value.pairing !== undefined &&
				(!isRecord(value.pairing) ||
					typeof value.pairing.hash !== "string" ||
					!/^[a-f0-9]{64}$/.test(value.pairing.hash) ||
					typeof value.pairing.expiresAt !== "number" ||
					!Number.isSafeInteger(value.pairing.expiresAt)))
		)
			throw new Error("Invalid Telegram connection settings. Run /telegram disconnect, then /telegram setup.");
		return value as unknown as TelegramConnectionSettings;
	}

	state(botId: number): TelegramState {
		const value = this.read("state.json");
		if (value === undefined || (isRecord(value) && value.botId !== botId))
			return { botId, offset: 0, outbox: [], inbox: [] };
		if (
			!isRecord(value) ||
			!Number.isSafeInteger(value.offset) ||
			(value.offset as number) < 0 ||
			!Array.isArray(value.outbox) ||
			value.outbox.length > 1000 ||
			!value.outbox.every(
				(item) =>
					isRecord(item) &&
					typeof item.id === "string" &&
					isTelegramId(item.chatId) &&
					typeof item.text === "string" &&
					item.text.length <= 4000,
			) ||
			!Array.isArray(value.inbox) ||
			value.inbox.length > 100 ||
			!value.inbox.every(
				(item) =>
					isRecord(item) &&
					isTelegramUpdateId(item.id) &&
					typeof item.text === "string" &&
					item.text.length <= 16384,
			) ||
			(value.lastAssistantKey !== undefined && typeof value.lastAssistantKey !== "string") ||
			(value.interruptedUpdate !== undefined && !isTelegramUpdateId(value.interruptedUpdate))
		)
			throw new Error("Invalid Telegram delivery state. Disconnect Telegram before repairing state.json.");
		return value as unknown as TelegramState;
	}

	status(): TelegramWorkerStatus | undefined {
		const value = this.read("worker.json");
		if (
			!isRecord(value) ||
			typeof value.instanceId !== "string" ||
			!isTelegramId(value.pid) ||
			typeof value.updatedAt !== "number" ||
			!["starting", "pairing", "running", "stopped", "error"].includes(String(value.phase))
		)
			return undefined;
		return value as unknown as TelegramWorkerStatus;
	}
}
