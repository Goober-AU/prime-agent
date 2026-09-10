import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import {
	Container,
	ProcessTerminal,
	setKeybindings,
	Text,
	truncateToWidth,
	visibleWidth,
} from "@earendil-works/pi-tui";
import stripAnsi from "strip-ansi";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeybindingsManager } from "../src/core/keybindings.js";
import type { ModelRegistry } from "../src/core/model-registry.js";
import { SettingsManager } from "../src/core/settings-manager.js";
import { AgentsViewMode } from "../src/modes/agents-view/agents-view-mode.js";
import type { SessionSummary } from "../src/modes/daemon/daemon-session-list.js";
import { InteractiveMode } from "../src/modes/interactive/interactive-mode.js";
import type { InteractiveModeUiServices } from "../src/modes/interactive/interactive-mode-services.js";
import { initTheme, stopThemeWatcher, Theme } from "../src/modes/interactive/theme/theme.js";
import { OPTIMUS_ART } from "../src/themes/optimus-logo-data.js";

vi.mock("../src/utils/tools-manager.js", () => ({
	ensureTool: vi.fn(async () => undefined),
	ensureToolWithStatus: vi.fn(async () => ({ status: "available", path: "fixture-rg" })),
	formatMissingRipgrepMessage: vi.fn(() => ""),
}));

function uiServices(): InteractiveModeUiServices {
	return {
		settingsManager: SettingsManager.inMemory({ theme: "dark", terminal: { fullscreen: false } }),
		modelRegistry: {} as ModelRegistry,
		getInitialCwd: () => process.cwd(),
		getInitialSessionName: () => undefined,
		getThemes: () => [],
	};
}

function savedSession(index: number): SessionSummary {
	return {
		id: `layout-${index}`,
		sessionId: `layout-${index}`,
		sessionFile: join(process.cwd(), `layout-${index}.jsonl`),
		sessionName: `Review ${index + 1}`,
		cwd: process.cwd(),
		lifecycle: "live",
		activity: "idle",
		isSessionActive: false,
		isStreaming: false,
		isCompacting: false,
		attachedClients: 0,
		messageCount: 1,
		rosterStatus: "inactive",
		sessionActions: { queuedCount: 0, steering: [], followUps: [] },
	};
}

function createBrowser(rows: number) {
	const terminal = { rows };
	const view = new AgentsViewMode(
		{ config: {}, uiServices: uiServices(), startupModelId: "astra" },
		{ savedCatalogLoaded: true },
	);
	Reflect.set(view, "ui", { terminal, requestRender: vi.fn() });
	Reflect.set(
		view,
		"lastListedSummaries",
		Array.from({ length: 6 }, (_, index) => savedSession(index)),
	);
	Reflect.get(AgentsViewMode.prototype, "reconcileCatalogs").call(view);
	return { view, terminal };
}

async function createWelcome(rows: number) {
	// Exercise the actual init caller and Container layout, but do not start a TUI,
	// connect to a daemon, load a user session, or register process signal handlers.
	const screen = new Container();
	const terminal = { rows };
	const headerContainer = new Container();
	const editorContainer = new Container();
	editorContainer.addChild(new Text("WRITE A PROMPT", 0, 0));
	const mode = Object.assign(Object.create(InteractiveMode.prototype) as InteractiveMode, {
		options: {},
		version: "0.9.3",
		uiServices: uiServices(),
		ui: {
			terminal,
			addChild: (child: Container) => screen.addChild(child),
			setFocus: vi.fn(),
			start: vi.fn(),
			invalidate: vi.fn(),
			requestRender: vi.fn(),
		},
		headerContainer,
		mainContainer: new Container(),
		mainViewContainer: new Container(),
		widgetContainerAbove: new Container(),
		widgetContainerBelow: new Container(),
		editorContainer,
		editor: {},
		subagentSummaryLine: new Text("", 0, 0),
		footerSlot: new Container(),
		footer: new Text("STATUS", 0, 0),
		promptDock: new Container(),
		footerDataProvider: { onBranchChange: vi.fn() },
		startHint: "Try a fixture prompt",
		getCurrentModelId: () => "astra",
		getCurrentCwd: () => process.cwd(),
		isNewChat: () => true,
		getPromptContextContainers: () => [],
		getPromptDockComponents: () => [],
		registerSignalHandlers: vi.fn(),
		renderWidgets: vi.fn(),
		renderRecap: vi.fn(),
		setupKeyHandlers: vi.fn(),
		setupEditorSubmitHandler: vi.fn(),
		rebindCurrentSession: vi.fn(async () => undefined),
		renderInitialMessages: vi.fn(async () => undefined),
		updateAvailableProviderCount: vi.fn(async () => undefined),
	});
	await InteractiveMode.prototype.init.call(mode);
	return { screen, headerContainer, terminal };
}

const portrait = OPTIMUS_ART.variants.find((variant) => variant.lines.length === 12)!;
const smallPortrait = OPTIMUS_ART.variants.find((variant) => variant.lines.length === 8)!;
const dotRows = (lines: string[]) => lines.filter((line) => /[\u2800-\u28ff]/.test(stripAnsi(line)));

function expectPortrait(lines: string[], expected: readonly string[]) {
	const rendered = dotRows(lines).map(stripAnsi);
	expect(rendered).toHaveLength(expected.length);
	expected.forEach((line, index) => {
		expect(rendered[index]).toContain(line);
	});
}

function savePreview(name: string, lines: string[]) {
	const root = process.env.PRIME_TEST_UI_PREVIEW_DIR;
	if (!root) return;
	mkdirSync(root, { recursive: true });
	writeFileSync(join(root, `${name}.ansi`), `${lines.join("\n")}\n`, "utf8");
}

describe("Optimus actual welcome and session-browser layout", () => {
	beforeEach(() => {
		vi.stubEnv("NO_COLOR", "");
		initTheme("dark");
		setKeybindings(new KeybindingsManager());
		vi.spyOn(ProcessTerminal.prototype, "setTitle").mockImplementation(() => {});
		vi.spyOn(Theme.prototype, "getColorMode").mockReturnValue("truecolor");
	});

	afterEach(() => {
		stopThemeWatcher();
		vi.restoreAllMocks();
		vi.unstubAllEnvs();
	});

	it("renders the approved complete compact portrait in the actual welcome caller", async () => {
		const { screen, headerContainer, terminal } = await createWelcome(40);
		for (const width of [80, 120]) {
			const header = headerContainer.render(width);
			expect(header.length).toBeLessThanOrEqual(14);
			expectPortrait(header, portrait.lines);
			const lines = screen.render(width);
			const output = stripAnsi(lines.join("\n"));
			for (const value of ["OPTIMUS", "version", "v0.9.3", "model", "astra", "cwd", "Try a fixture prompt"]) {
				expect(output).toContain(value);
			}
			expect(lines.findIndex((line) => line.includes("WRITE A PROMPT"))).toBeLessThanOrEqual(14);
			lines.forEach((line) => {
				expect(visibleWidth(line)).toBeLessThanOrEqual(width);
			});
			savePreview(`welcome-${width}x40`, lines);
		}
		terminal.rows = 24;
		expectPortrait(headerContainer.render(80), smallPortrait.lines);
		terminal.rows = 18;
		const small = screen.render(80);
		expect(dotRows(small)).toHaveLength(0);
		expect(small.length).toBeLessThanOrEqual(18);
		expect(stripAnsi(small.join("\n"))).toContain("WRITE A PROMPT");
		savePreview("welcome-80x18", small);
	});

	it("keeps the whole browser header within 14 rows and renders useful session rows", () => {
		const { view } = createBrowser(40);
		for (const width of [80, 120]) {
			const lines = view.render(width);
			expectPortrait(lines, portrait.lines);
			const output = stripAnsi(lines.join("\n"));
			for (const value of ["OPTIMUS", "astra", "version", "cwd", "agents", "scope", "depth", "global"]) {
				expect(output).toContain(value);
			}
			const promptRows = Reflect.get(AgentsViewMode.prototype, "renderPrompt").call(view, width) as string[];
			expect(lines.findIndex((line) => stripAnsi(line) === stripAnsi(promptRows[0]))).toBeLessThanOrEqual(14);
			expect(lines.filter((line) => /\bReview [1-6]\b/.test(stripAnsi(line))).length).toBeGreaterThanOrEqual(3);
			lines.forEach((line) => {
				expect(visibleWidth(line)).toBeLessThanOrEqual(width);
			});
			savePreview(`browser-${width}x40`, lines);
		}
	});

	it.each([
		[80, 24],
		[80, 18],
		[40, 18],
		[80, 12],
	])("shrinks or omits art before search and session rows at %ix%i", (width, rows) => {
		const { view, terminal } = createBrowser(40);
		terminal.rows = rows;
		const lines = view.render(width);
		const art = dotRows(lines);
		if (art.length > 0) expectPortrait(lines, smallPortrait.lines);
		else expect(art).toHaveLength(0);
		const promptRows = Reflect.get(AgentsViewMode.prototype, "renderPrompt").call(view, width) as string[];
		const output = stripAnsi(lines.join("\n"));
		for (const prompt of promptRows) expect(output).toContain(stripAnsi(prompt));
		expect(output).toContain("Review 1");
		expect(lines.length).toBeLessThanOrEqual(rows);
		lines.forEach((line) => {
			expect(visibleWidth(line)).toBeLessThanOrEqual(width);
		});
		savePreview(`browser-${width}x${rows}`, lines);
	});

	it.each(["truecolor", "256color", "none"] as const)(
		"retains %s rendering and light-theme contrast",
		async (mode) => {
			vi.spyOn(Theme.prototype, "getColorMode").mockReturnValue(mode === "256color" ? "256color" : "truecolor");
			vi.stubEnv("NO_COLOR", mode === "none" ? "1" : "");
			const { screen } = await createWelcome(40);
			const dark = screen.render(80);
			expectPortrait(dark, portrait.lines);
			// Keep the art's ANSI sequences, excluding the separately themed metadata.
			const portraitWidth = 1 + Math.max(...portrait.lines.map((line) => visibleWidth(line)));
			const artRegion = (lines: string[]) => dotRows(lines).map((line) => truncateToWidth(line, portraitWidth, ""));
			const darkArt = artRegion(dark);
			if (mode === "none") expect(darkArt.join("\n")).not.toMatch(/\x1b\[38;/);
			else expect(darkArt.join("\n")).toContain(mode === "truecolor" ? "\x1b[38;2;" : "\x1b[38;5;");
			for (const value of ["OPTIMUS", "version", "model", "astra", "cwd"]) {
				expect(stripAnsi(dark.join("\n"))).toContain(value);
			}
			initTheme("light");
			const lightArt = artRegion(screen.render(80));
			expect(lightArt.map(stripAnsi)).toEqual(darkArt.map(stripAnsi));
			if (mode === "none") expect(lightArt.join("\n")).not.toMatch(/\x1b\[38;/);
			else expect(lightArt).not.toEqual(darkArt);
		},
	);
});
