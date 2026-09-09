import { rmSync } from "node:fs";
import { defaultDaemonSocketPath } from "../../../modes/daemon/daemon-socket.js";
import { DAEMON_WORKER_SUPERVISOR_SOCKET_ENV } from "../../../modes/daemon/daemon-worker-protocol.js";
import { TelegramApi } from "../../../modes/telegram/api.js";
import {
	describeTelegram,
	startTelegramWorker,
	stopTelegramWorker,
	withTelegramManagement,
} from "../../../modes/telegram/manager.js";
import {
	createPairing,
	type TelegramConnectionSettings,
	TelegramStore,
	validBotToken,
} from "../../../modes/telegram/store.js";
import type { ExtensionCommandContext, ExtensionFactory } from "../types.js";

export const TELEGRAM_SETUP_INSTRUCTIONS = [
	"1. Open https://t.me/BotFather in Telegram (the official @BotFather).",
	"2. Send /newbot, choose a display name, and choose a username ending in bot.",
	"3. Copy its bot token and paste it into the next Prime input dialog.",
	"4. Open the pairing link Prime provides and press Start in your bot's private chat.",
	"The paired Telegram account can control your Prime sessions and tools. Keep the token and pairing link private.",
].join("\n");

function sessionBinding(
	ctx: ExtensionCommandContext,
): Pick<TelegramConnectionSettings, "cwd" | "sessionId" | "sessionFile" | "daemonSocket"> {
	return {
		cwd: ctx.cwd,
		sessionId: ctx.sessionManager.getSessionId(),
		sessionFile: ctx.sessionManager.getSessionFile(),
		daemonSocket: process.env[DAEMON_WORKER_SUPERVISOR_SOCKET_ENV] || defaultDaemonSocketPath(),
	};
}

export function createTelegramExtension(agentDir: string): ExtensionFactory {
	return (pi) => {
		const store = new TelegramStore(agentDir);
		pi.registerCommand("telegram", {
			description: "Connect Telegram to Prime, with BotFather setup and private pairing",
			getArgumentCompletions: (prefix) =>
				["setup", "status", "pair", "here", "restart", "pause", "disconnect"]
					.filter((value) => value.startsWith(prefix))
					.map((value) => ({ value, label: value })),
			handler: async (args, ctx) => {
				if (!ctx.hasUI) throw new Error("Run /telegram in the Prime terminal to manage the connection.");
				let action = args.trim();
				if (!action) {
					const choice = await ctx.ui.select("Telegram — connect to Prime", [
						"Setup with BotFather",
						"Connection status",
						"Pair account",
						"Connect this session",
						"Restart",
						"Pause",
						"Disconnect",
					]);
					if (!choice) return;
					action = (
						{
							"Setup with BotFather": "setup",
							"Connection status": "status",
							"Pair account": "pair",
							"Connect this session": "here",
							Restart: "restart",
							Pause: "pause",
							Disconnect: "disconnect",
						} as Record<string, string>
					)[choice];
				}
				if (action === "status") {
					ctx.ui.notify(await describeTelegram(store), "info");
					return;
				}
				if (!["setup", "pair", "here", "restart", "pause", "disconnect"].includes(action)) {
					ctx.ui.notify(
						"Usage: /telegram [setup|status|pair|here|restart|pause|disconnect]. Paste bot tokens only into the setup dialog.",
						"error",
					);
					return;
				}
				await withTelegramManagement(store, async () => {
					await stopTelegramWorker(store);
					let prepared: TelegramConnectionSettings | undefined;
					if (action === "setup") {
						try {
							ctx.ui.notify(TELEGRAM_SETUP_INSTRUCTIONS, "info");
							const token = (
								await ctx.ui.input("Telegram bot token from @BotFather (not saved to chat history)")
							)?.trim();
							if (!token) return;
							if (!validBotToken(token)) {
								ctx.ui.notify("That does not look like a BotFather token. Run /telegram setup again.", "error");
								return;
							}
							const api = new TelegramApi(token);
							const bot = await api.identify();
							await api.requirePolling();
							prepared = {
								version: 1,
								enabled: true,
								botToken: token,
								botId: bot.id,
								botUsername: bot.username,
								...sessionBinding(ctx),
							};
						} finally {
							if (!prepared) {
								await startTelegramWorker(store).catch(() => {
									ctx.ui.notify("Telegram remains stopped. Use /telegram restart to reconnect.", "warning");
								});
							}
						}
					}
					if (action === "disconnect") {
						for (const name of ["connection.json", "state.json", "worker.json", "stop.json"])
							rmSync(store.path(name), { force: true });
						ctx.ui.notify("Telegram disconnected and its local credentials removed.", "info");
						return;
					}
					const settings = prepared ?? store.settings();
					if (!settings) {
						ctx.ui.notify("Use /telegram setup to connect a bot first.", "info");
						return;
					}
					let pairingLink: string | undefined;
					if (action === "setup" || action === "pair") {
						if (settings.pairedUserId) {
							await startTelegramWorker(store);
							ctx.ui.notify("An account is already paired. Disconnect and set up again to replace it.", "info");
							return;
						}
						const pairing = createPairing();
						settings.pairing = pairing.pairing;
						pairingLink = `https://t.me/${settings.botUsername}?start=${pairing.code}`;
					}
					if (action === "setup") rmSync(store.path("state.json"), { force: true });
					if (action === "here") Object.assign(settings, sessionBinding(ctx));
					settings.enabled = action !== "pause";
					store.write("connection.json", settings);
					if (settings.enabled) await startTelegramWorker(store);
					ctx.ui.notify(
						pairingLink
							? `Open this one-time link in Telegram and press Start (expires in 10 minutes):\n${pairingLink}\nUse /telegram pair for a fresh link.`
							: await describeTelegram(store),
						"info",
					);
				});
			},
		});
		pi.on("session_start", (_event, ctx) => {
			void (async () => {
				if (!store.settings()?.enabled) return;
				await withTelegramManagement(store, () => startTelegramWorker(store));
			})().catch(() => {
				if (ctx.hasUI)
					ctx.ui.notify("Telegram is unavailable. Use /telegram status to inspect the connection.", "warning");
			});
		});
	};
}
