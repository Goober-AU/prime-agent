import { isRecord, isTelegramId, isTelegramUpdateId, validBotToken } from "./store.js";

export class TelegramApiError extends Error {
	constructor(
		readonly code: number,
		readonly retryAfter = 0,
	) {
		super(
			code === 401
				? "Telegram rejected the bot token. Create a new token in BotFather and run /telegram setup."
				: code === 409
					? "Another Telegram poller or webhook is using this bot. Stop it or create a separate bot for Prime."
					: code === 403
						? "Telegram delivery was blocked. Open the bot chat and unblock the bot."
						: `Telegram request failed (${code}).`,
		);
	}
}

export interface TelegramMessage {
	message_id: number;
	date: number;
	from: { id: number; is_bot: boolean };
	chat: { id: number; type: string };
	text?: string;
}

export interface TelegramUpdate {
	update_id: number;
	message?: TelegramMessage;
}

export function privateMessage(update: unknown): TelegramMessage | undefined {
	if (!isRecord(update) || !isRecord(update.message)) return undefined;
	const message = update.message;
	if (
		!isRecord(message.from) ||
		!isRecord(message.chat) ||
		message.chat.type !== "private" ||
		message.from.is_bot !== false ||
		!isTelegramId(message.from.id) ||
		message.chat.id !== message.from.id ||
		!isTelegramId(message.message_id) ||
		!Number.isSafeInteger(message.date) ||
		(message.text !== undefined && (typeof message.text !== "string" || message.text.length > 16384))
	)
		return undefined;
	return message as unknown as TelegramMessage;
}

export function splitTelegramText(text: string, limit = 4000): string[] {
	if (!Number.isSafeInteger(limit) || limit < 2 || limit > 4096) throw new Error("Invalid Telegram message limit.");
	const chunks: string[] = [];
	let chunk = "";
	for (const character of text) {
		if (chunk.length + character.length > limit) {
			chunks.push(chunk);
			chunk = "";
		}
		chunk += character;
	}
	if (chunk) chunks.push(chunk);
	return chunks;
}

export class TelegramApi {
	constructor(
		private readonly token: string,
		private readonly baseUrl = "https://api.telegram.org",
		private readonly fetcher: typeof fetch = fetch,
	) {
		if (!validBotToken(token)) throw new Error("Invalid Telegram bot token format.");
	}

	redact(error: unknown): string {
		return (error instanceof Error ? error.message : "Telegram operation failed.").replaceAll(
			this.token,
			"[redacted]",
		);
	}

	async call(method: string, body: Record<string, unknown> = {}, signal?: AbortSignal): Promise<unknown> {
		let response: Response;
		try {
			response = await this.fetcher(`${this.baseUrl}/bot${this.token}/${method}`, {
				method: "POST",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify(body),
				signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(40_000)]) : AbortSignal.timeout(15_000),
			});
		} catch {
			if (signal?.aborted) throw signal.reason;
			throw new Error("Cannot reach Telegram. Check this computer's internet connection.");
		}
		const reader = response.body?.getReader();
		let bytes = 0;
		const chunks: Uint8Array[] = [];
		try {
			while (reader) {
				const next = await reader.read();
				if (next.done) break;
				bytes += next.value.byteLength;
				if (bytes > 2 * 1024 * 1024) throw new Error("Telegram response was too large.");
				chunks.push(next.value);
			}
		} finally {
			await reader?.cancel().catch(() => undefined);
		}
		let data: unknown;
		try {
			data = JSON.parse(Buffer.concat(chunks).toString("utf8"));
		} catch {
			throw new Error("Telegram returned an invalid response.");
		}
		if (!isRecord(data) || !response.ok || data.ok !== true) {
			const code = isRecord(data) && typeof data.error_code === "number" ? data.error_code : response.status;
			const retry =
				isRecord(data) && isRecord(data.parameters) && typeof data.parameters.retry_after === "number"
					? Math.max(0, Math.min(300, data.parameters.retry_after))
					: 0;
			throw new TelegramApiError(code, retry);
		}
		return data.result;
	}

	async identify(signal?: AbortSignal): Promise<{ id: number; username: string }> {
		const result = await this.call("getMe", {}, signal);
		if (
			!isRecord(result) ||
			!isTelegramId(result.id) ||
			result.is_bot !== true ||
			typeof result.username !== "string" ||
			!/^[A-Za-z0-9_]{1,64}$/.test(result.username)
		)
			throw new Error("Telegram returned an invalid bot identity.");
		return { id: result.id, username: result.username };
	}

	async requirePolling(signal?: AbortSignal): Promise<void> {
		const result = await this.call("getWebhookInfo", {}, signal);
		if (!isRecord(result) || typeof result.url !== "string")
			throw new Error("Telegram returned invalid webhook information.");
		if (result.url)
			throw new Error(
				"This bot already has a webhook. Disconnect its other integration or create a new bot for Prime in BotFather.",
			);
	}

	async updates(offset: number, signal: AbortSignal): Promise<TelegramUpdate[]> {
		const result = await this.call(
			"getUpdates",
			{ offset, timeout: 25, limit: 50, allowed_updates: ["message"] },
			signal,
		);
		if (!Array.isArray(result) || !result.every((update) => isRecord(update) && isTelegramUpdateId(update.update_id)))
			throw new Error("Telegram returned invalid updates.");
		return result as TelegramUpdate[];
	}

	async send(chatId: number, text: string, signal?: AbortSignal): Promise<void> {
		await this.call("sendMessage", { chat_id: chatId, text, link_preview_options: { is_disabled: true } }, signal);
	}
}
