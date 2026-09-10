/**
 * Consolidated baseline-vs-combined comparator for the synthetic large-history fixture.
 *
 * This helper does not attribute a result to one fix. It must run only after the
 * packaging owner supplies separately built, immutable baseline and combined
 * coding-agent roots. Each measured sample runs in a fresh child process.
 */
import { createHash } from "node:crypto";
import { once } from "node:events";
import { createReadStream, existsSync } from "node:fs";
import { copyFile, mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createServer, createConnection } from "node:net";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { createRequire } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawn } from "node:child_process";

const REQUIRED_BASELINE_ID = "225769366d65bade9ce5a6c1ecf4fc1a593dd10c";
const MIN_FIXTURE_BYTES = 30 * 1024 * 1024;
const MAX_FIXTURE_BYTES = 80 * 1024 * 1024;
const HISTORY_WINDOW_MESSAGES = 400;
const SAMPLE_PREFIX = "PRIME_HISTORY_SAMPLE=";

type Arm = "baseline" | "combined";
type Phase = "warmup" | "measured";

interface ComparatorOptions {
	baselineRoot: string;
	combinedRoot: string;
	baselineId: string;
	combinedId: string;
	fixture: string;
	manifest: string;
	output?: string;
	workRoot?: string;
	warmups: number;
	trials: number;
	width: number;
	keepWork: boolean;
}

interface WorkerOptions {
	arm: Arm;
	runtimeRoot: string;
	fixture: string;
	width: number;
}

interface HistoryManifest {
	fixture: string;
	synthetic: true;
	productionData: false;
	bytes: number;
	mib?: number;
	messageCount?: number;
	toolCycles?: number;
}

interface HistoryWindowLike {
	version: number;
	generation: string;
	representation: string;
	tipEntryId: string | null;
	totalMessageCount: number;
	startIndex: number;
	messages: unknown[];
	entryIds: string[];
	hasOlder: boolean;
	order: string;
}

interface SessionManagerLike {
	buildSessionContext(): {
		messages: unknown[];
		thinkingLevel: unknown;
		serviceTier: unknown;
		model: unknown;
	};
	buildSessionHistory?: () => {
		messages: unknown[];
		entryIds: string[];
		tipEntryId: string | null;
	};
	getLoadObservation?: () => { readBytes: number } | undefined;
}

interface SessionFileEntryLike {
	type?: unknown;
	cwd?: unknown;
}

interface SessionManagerModuleLike {
	SessionManager: new (
		cwd: string,
		sessionDir: string,
		sessionFile: string,
		persist: boolean,
		preloadedEntries: SessionFileEntryLike[],
	) => SessionManagerLike;
	loadEntriesFromFileAsync(path: string): Promise<SessionFileEntryLike[]>;
}

interface DaemonModeModuleLike {
	slicePinnedSessionHistory?: (
		history: ReturnType<NonNullable<SessionManagerLike["buildSessionHistory"]>>,
		options: { generation: string; representation: string; limit: number },
	) => HistoryWindowLike;
}

interface JsonlModuleLike {
	serializeJsonLine(value: unknown): string;
}

interface InteractiveModuleLike {
	InteractiveMode: {
		prototype: Record<string, unknown>;
	};
}

interface ThemeModuleLike {
	initTheme(name?: string, watch?: boolean): void;
}

interface ContainerLike {
	children: unknown[];
	render(width: number): string[];
}

interface TuiModuleLike {
	Container: new () => ContainerLike;
}

interface MemoryPoint {
	phase: string;
	rssBytes: number;
	heapUsedBytes: number;
}

interface HistorySample {
	arm: Arm;
	fixtureBytes: number;
	primaryReadBytes: number | null;
	primaryFullFileParseMs: number;
	managerHydrationMs: number;
	snapshotBuildMs: number;
	serializationMs: number;
	transportMs: number;
	wireBytes: number;
	receivedBytes: number;
	firstUsableRenderMs: number;
	terminalFirstPaintMs: null;
	lifetimeMessageCount: number;
	visibleMessageCount: number;
	renderedLineCount: number;
	renderedUtf8Bytes: number;
	renderRequestCount: number;
	processMaxRssBytes: number | null;
	boundaryMaxRssBytes: number;
	boundaryMaxHeapUsedBytes: number;
	memoryPoints: MemoryPoint[];
}

interface NumericDistribution {
	samples: number;
	median: number;
	min: number;
	max: number;
}

interface NullableDistribution {
	availableSamples: number;
	distribution: NumericDistribution | null;
	unavailableReason?: string;
}

function positiveInteger(value: string | undefined, fallback: number): number {
	const parsed = Number.parseInt(value ?? "", 10);
	return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback;
}

function requireValue(argv: string[], index: number, flag: string): string {
	const value = argv[index + 1];
	if (!value || value.startsWith("--")) throw new Error(`${flag} requires a value`);
	return value;
}

function parseComparatorOptions(argv: string[]): ComparatorOptions {
	const parsed: Partial<ComparatorOptions> = {
		baselineId: REQUIRED_BASELINE_ID,
		warmups: 1,
		trials: 5,
		width: 120,
		keepWork: false,
	};
	for (let index = 0; index < argv.length; index++) {
		const argument = argv[index];
		if (argument === "--baseline-root") parsed.baselineRoot = requireValue(argv, index++, argument);
		else if (argument === "--combined-root") parsed.combinedRoot = requireValue(argv, index++, argument);
		else if (argument === "--baseline-id") parsed.baselineId = requireValue(argv, index++, argument);
		else if (argument === "--combined-id") parsed.combinedId = requireValue(argv, index++, argument);
		else if (argument === "--fixture") parsed.fixture = requireValue(argv, index++, argument);
		else if (argument === "--manifest") parsed.manifest = requireValue(argv, index++, argument);
		else if (argument === "--output") parsed.output = requireValue(argv, index++, argument);
		else if (argument === "--work-root") parsed.workRoot = requireValue(argv, index++, argument);
		else if (argument === "--warmups") parsed.warmups = positiveInteger(requireValue(argv, index++, argument), 1);
		else if (argument === "--trials") parsed.trials = positiveInteger(requireValue(argv, index++, argument), 5);
		else if (argument === "--width") parsed.width = positiveInteger(requireValue(argv, index++, argument), 120);
		else if (argument === "--keep-work") parsed.keepWork = true;
		else if (argument === "--help" || argument === "-h") {
			process.stdout.write(
				"Usage: benchmark-history-reopen.ts --baseline-root DIR --combined-root DIR " +
					"--combined-id ID --fixture FILE --manifest FILE [--output FILE] " +
					"[--work-root DIR] [--warmups 1] [--trials 5] [--width 120] [--keep-work]\n",
			);
			process.exit(0);
		} else throw new Error(`Unknown argument: ${argument}`);
	}
	if (!parsed.baselineRoot || !parsed.combinedRoot || !parsed.combinedId || !parsed.fixture || !parsed.manifest) {
		throw new Error("baseline root, combined root/id, fixture, and manifest are required");
	}
	if (parsed.baselineId !== REQUIRED_BASELINE_ID) {
		throw new Error(`Baseline id must remain the unchanged base ${REQUIRED_BASELINE_ID}`);
	}
	const options = parsed as ComparatorOptions;
	options.baselineRoot = resolve(options.baselineRoot);
	options.combinedRoot = resolve(options.combinedRoot);
	options.fixture = resolve(options.fixture);
	options.manifest = resolve(options.manifest);
	if (options.output) options.output = resolve(options.output);
	if (options.workRoot) options.workRoot = resolve(options.workRoot);
	if (options.baselineRoot === options.combinedRoot) throw new Error("Baseline and combined runtime roots must differ");
	options.warmups = Math.min(options.warmups, 2);
	options.trials = Math.max(3, Math.min(options.trials, 9));
	options.width = Math.max(80, Math.min(options.width, 240));
	return options;
}

function parseWorkerOptions(argv: string[]): WorkerOptions {
	const parsed: Partial<WorkerOptions> = {};
	for (let index = 0; index < argv.length; index++) {
		const argument = argv[index];
		if (argument === "--arm") parsed.arm = requireValue(argv, index++, argument) as Arm;
		else if (argument === "--runtime-root") parsed.runtimeRoot = requireValue(argv, index++, argument);
		else if (argument === "--fixture") parsed.fixture = requireValue(argv, index++, argument);
		else if (argument === "--width") parsed.width = positiveInteger(requireValue(argv, index++, argument), 120);
		else throw new Error(`Unknown worker argument: ${argument}`);
	}
	if ((parsed.arm !== "baseline" && parsed.arm !== "combined") || !parsed.runtimeRoot || !parsed.fixture) {
		throw new Error("worker arm, runtime root, and fixture are required");
	}
	return {
		arm: parsed.arm,
		runtimeRoot: resolve(parsed.runtimeRoot),
		fixture: resolve(parsed.fixture),
		width: Math.max(80, Math.min(parsed.width ?? 120, 240)),
	};
}

async function sha256(path: string): Promise<string> {
	const hash = createHash("sha256");
	const stream = createReadStream(path);
	stream.on("data", (chunk) => hash.update(chunk));
	await once(stream, "end");
	return hash.digest("hex");
}

async function validateSyntheticFixture(fixture: string, manifestPath: string): Promise<HistoryManifest> {
	const info = await stat(fixture);
	if (!info.isFile() || info.size < MIN_FIXTURE_BYTES || info.size > MAX_FIXTURE_BYTES) {
		throw new Error("History fixture must be a 30-80 MiB regular file");
	}
	const value = JSON.parse(await readFile(manifestPath, "utf8")) as Partial<HistoryManifest>;
	if (value.synthetic !== true || value.productionData !== false) {
		throw new Error("Comparator refuses a fixture not explicitly marked synthetic and non-production");
	}
	if (value.fixture !== basename(fixture) || value.bytes !== info.size) {
		throw new Error("Fixture path/size does not match its synthetic manifest");
	}
	return value as HistoryManifest;
}

function runtimeModulePath(runtimeRoot: string, relativePath: string): string {
	const path = join(runtimeRoot, "dist", ...relativePath.split("/"));
	if (!existsSync(path)) throw new Error(`Built runtime module is missing: ${path}`);
	return path;
}

async function importRuntimeModule<T>(runtimeRoot: string, relativePath: string): Promise<T> {
	return (await import(pathToFileURL(runtimeModulePath(runtimeRoot, relativePath)).href)) as T;
}

function memoryPoint(phase: string): MemoryPoint {
	const usage = process.memoryUsage();
	return { phase, rssBytes: usage.rss, heapUsedBytes: usage.heapUsed };
}

async function transferOverLoopback(payload: Buffer): Promise<{ bytes: number; durationMs: number }> {
	let receivedBytes = 0;
	let resolveReceived!: () => void;
	let rejectReceived!: (error: Error) => void;
	const received = new Promise<void>((resolvePromise, rejectPromise) => {
		resolveReceived = resolvePromise;
		rejectReceived = rejectPromise;
	});
	const server = createServer((socket) => {
		socket.on("data", (chunk: Buffer) => {
			receivedBytes += chunk.length;
		});
		socket.once("end", resolveReceived);
		socket.once("error", rejectReceived);
	});
	server.listen(0, "127.0.0.1");
	await once(server, "listening");
	const address = server.address();
	if (!address || typeof address === "string") throw new Error("Loopback benchmark did not receive a TCP port");
	const client = createConnection({ host: "127.0.0.1", port: address.port });
	try {
		await once(client, "connect");
		const startedAt = performance.now();
		client.end(payload);
		await received;
		return { bytes: receivedBytes, durationMs: performance.now() - startedAt };
	} finally {
		client.destroy();
		server.close();
		await once(server, "close");
	}
}

async function createRenderHarness(runtimeRoot: string, fixture: string): Promise<{
	renderSessionContext: (
		context: Record<string, unknown>,
		options: Record<string, unknown>,
	) => Promise<void>;
	container: ContainerLike;
	getRenderRequests(): number;
}> {
	const interactive = await importRuntimeModule<InteractiveModuleLike>(
		runtimeRoot,
		"modes/interactive/interactive-mode.js",
	);
	const theme = await importRuntimeModule<ThemeModuleLike>(runtimeRoot, "modes/interactive/theme/theme.js");
	theme.initTheme("dark", false);
	const runtimeRequire = createRequire(join(runtimeRoot, "package.json"));
	const tuiEntry = runtimeRequire.resolve("@earendil-works/pi-tui");
	const tui = (await import(pathToFileURL(tuiEntry).href)) as TuiModuleLike;
	const container = new tui.Container();
	const prototype = interactive.InteractiveMode.prototype;
	const render = prototype.renderSessionContext;
	if (typeof render !== "function") throw new Error("Runtime does not expose its existing renderSessionContext path");
	let renderRequests = 0;
	const harness = Object.create(prototype) as Record<string, unknown>;
	const harnessProperties: Record<string, unknown> = {
		options: { verbose: false },
		chatContainer: container,
		pendingTools: new Map(),
		pendingToolGeneration: 0,
		pendingToolCreations: new Set(),
		startedToolCalls: new Set(),
		ipythonToolComponents: new Map(),
		lateIpythonSentAgentMessages: new Map(),
		toolDefinitionCache: new Map(),
		toolOutputExpanded: false,
		agentMessagesExpanded: false,
		editDiffsExpanded: false,
		hideThinkingBlock: false,
		hiddenThinkingLabel: undefined,
		mermaidMarkdownTransform: undefined,
		bindLocalSessionExtensions: false,
		localSessionHost: undefined,
		connectionCommands: [],
		connectionState: undefined,
		agentConnection: { getToolDefinition: async () => undefined },
		settingsManager: {
			getShowImages: () => false,
			getCodeBlockIndent: () => "  ",
		},
		ui: { requestRender: () => renderRequests++ },
		editor: { addToHistory: () => undefined },
		footer: { invalidate: () => undefined },
		updateEditorBorderColor: () => undefined,
		getCurrentCwd: () => dirname(fixture),
	};
	// InteractiveMode has getter-backed public properties. Define every harness
	// stub as an own data property instead of invoking inherited accessors.
	for (const [name, value] of Object.entries(harnessProperties)) {
		Object.defineProperty(harness, name, { configurable: true, enumerable: true, value, writable: true });
	}
	return {
		renderSessionContext: (context, options) =>
			(render as (this: Record<string, unknown>, context: Record<string, unknown>, options: Record<string, unknown>) => Promise<void>).call(
				harness,
				context,
				options,
			),
		container,
		getRenderRequests: () => renderRequests,
	};
}

async function runWorker(options: WorkerOptions): Promise<HistorySample> {
	const fixtureInfo = await stat(options.fixture);
	const sessionModule = await importRuntimeModule<SessionManagerModuleLike>(options.runtimeRoot, "core/session-manager.js");
	const jsonlModule = await importRuntimeModule<JsonlModuleLike>(options.runtimeRoot, "modes/rpc/jsonl.js");
	const renderHarness = await createRenderHarness(options.runtimeRoot, options.fixture);
	(globalThis as { gc?: () => void }).gc?.();
	const memoryPoints: MemoryPoint[] = [memoryPoint("before_parse")];

	const parseStartedAt = performance.now();
	const entries = await sessionModule.loadEntriesFromFileAsync(options.fixture);
	const primaryFullFileParseMs = performance.now() - parseStartedAt;
	memoryPoints.push(memoryPoint("after_primary_full_file_parse"));
	const header = entries.find((entry) => entry.type === "session");
	const cwd = typeof header?.cwd === "string" ? header.cwd : dirname(options.fixture);
	const hydrationStartedAt = performance.now();
	const manager = new sessionModule.SessionManager(cwd, dirname(options.fixture), options.fixture, false, entries);
	const managerHydrationMs = performance.now() - hydrationStartedAt;
	memoryPoints.push(memoryPoint("after_manager_hydration"));
	const loadObservation = manager.getLoadObservation?.();

	const snapshotStartedAt = performance.now();
	const fullContext = manager.buildSessionContext();
	const lifetimeMessageCount = fullContext.messages.length;
	let visibleMessages = fullContext.messages;
	let historyWindow: Omit<HistoryWindowLike, "messages"> | undefined;
	if (options.arm === "combined") {
		if (!manager.buildSessionHistory) throw new Error("Combined runtime is missing buildSessionHistory");
		const daemonMode = await importRuntimeModule<DaemonModeModuleLike>(options.runtimeRoot, "modes/daemon/daemon-mode.js");
		if (!daemonMode.slicePinnedSessionHistory) {
			throw new Error("Combined runtime is missing slicePinnedSessionHistory");
		}
		const sliced = daemonMode.slicePinnedSessionHistory(manager.buildSessionHistory(), {
			generation: "synthetic-history-generation",
			representation: "synthetic-history-representation",
			limit: HISTORY_WINDOW_MESSAGES,
		});
		visibleMessages = sliced.messages;
		const { messages: _messages, ...window } = sliced;
		historyWindow = window;
	}
	const snapshotBuildMs = performance.now() - snapshotStartedAt;
	memoryPoints.push(memoryPoint("after_snapshot_build"));

	const serializationStartedAt = performance.now();
	const wire = Buffer.from(
		jsonlModule.serializeJsonLine({
			type: "session_attached",
			activeSessionId: "synthetic-active-session",
			snapshot: {
				activeSessionId: "synthetic-active-session",
				summary: { messageCount: lifetimeMessageCount },
				state: { messageCount: lifetimeMessageCount },
				messages: visibleMessages,
				...(historyWindow ? { history: historyWindow } : {}),
				lastEventSequence: 0,
				lastEventCursor: { generation: "synthetic-history-generation", sequence: 0 },
			},
		}),
		"utf8",
	);
	const serializationMs = performance.now() - serializationStartedAt;
	memoryPoints.push(memoryPoint("after_serialization"));
	const transport = await transferOverLoopback(wire);
	if (transport.bytes !== wire.length) throw new Error("Loopback transport byte count did not match wire payload");
	memoryPoints.push(memoryPoint("after_transport"));

	const renderContext = { ...fullContext, messages: visibleMessages } as Record<string, unknown>;
	const renderOptions: Record<string, unknown> =
		options.arm === "baseline"
			? { clearChat: true, limitTranscript: true }
			: {
					clearChat: true,
					messagesAlreadyChronological: true,
					totalMessageCount: lifetimeMessageCount,
				};
	const renderStartedAt = performance.now();
	await renderHarness.renderSessionContext(renderContext, renderOptions);
	const renderedLines = renderHarness.container.render(options.width);
	const firstUsableRenderMs = performance.now() - renderStartedAt;
	memoryPoints.push(memoryPoint("after_first_usable_component_render"));

	const maxRssKilobytes = process.resourceUsage().maxRSS;
	return {
		arm: options.arm,
		fixtureBytes: fixtureInfo.size,
		primaryReadBytes:
			loadObservation && Number.isFinite(loadObservation.readBytes) ? loadObservation.readBytes : null,
		primaryFullFileParseMs,
		managerHydrationMs,
		snapshotBuildMs,
		serializationMs,
		transportMs: transport.durationMs,
		wireBytes: wire.length,
		receivedBytes: transport.bytes,
		firstUsableRenderMs,
		terminalFirstPaintMs: null,
		lifetimeMessageCount,
		visibleMessageCount: visibleMessages.length,
		renderedLineCount: renderedLines.length,
		renderedUtf8Bytes: Buffer.byteLength(renderedLines.join("\n"), "utf8"),
		renderRequestCount: renderHarness.getRenderRequests(),
		processMaxRssBytes: Number.isFinite(maxRssKilobytes) && maxRssKilobytes > 0 ? maxRssKilobytes * 1024 : null,
		boundaryMaxRssBytes: Math.max(...memoryPoints.map((point) => point.rssBytes)),
		boundaryMaxHeapUsedBytes: Math.max(...memoryPoints.map((point) => point.heapUsedBytes)),
		memoryPoints,
	};
}

function sanitizedWorkerEnvironment(sampleRoot: string): NodeJS.ProcessEnv {
	const environment = { ...process.env };
	const unsafeName =
		/(?:API[_-]?KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL|OPENAI|ANTHROPIC|AZURE|AWS_|GOOGLE_|GEMINI|COPILOT|PRIME_(?:API|INFERENCE)|(?:SOCKET|KERNEL|BRIDGE))/i;
	for (const key of Object.keys(environment)) {
		if (key === "NODE_OPTIONS" || unsafeName.test(key)) delete environment[key];
	}
	const home = join(sampleRoot, "home");
	const temp = join(sampleRoot, "temp");
	const agent = join(sampleRoot, "agent");
	const sessions = join(sampleRoot, "sessions");
	environment.HOME = home;
	environment.USERPROFILE = home;
	environment.APPDATA = join(sampleRoot, "appdata");
	environment.LOCALAPPDATA = join(sampleRoot, "localappdata");
	environment.XDG_CONFIG_HOME = join(sampleRoot, "config");
	environment.XDG_CACHE_HOME = join(sampleRoot, "cache");
	environment.TEMP = temp;
	environment.TMP = temp;
	environment.PRIME_AGENT_CODING_AGENT_DIR = agent;
	environment.PRIME_AGENT_SESSION_DIR = sessions;
	environment.PRIME_AGENT_ARTIFACT_DIR = join(sampleRoot, "artifacts");
	environment.PRIME_AGENT_INTERNAL_DAEMON_SUPERVISOR_REGISTRY_DIR = join(sampleRoot, "owners");
	environment.PRIME_AGENT_DAEMON_SOCKET = join(sampleRoot, "benchmark-daemon.sock");
	environment.PRIME_AGENT_PERFORMANCE_METRICS = "0";
	environment.NO_COLOR = "1";
	environment.CI = "1";
	return environment;
}

async function runChildSample(
	arm: Arm,
	runtimeRoot: string,
	fixture: string,
	width: number,
	sampleRoot: string,
): Promise<HistorySample> {
	await Promise.all(
		["home", "temp", "agent", "sessions", "owners", "appdata", "localappdata", "config", "cache", "artifacts"].map(
			(name) => mkdir(join(sampleRoot, name), { recursive: true }),
		),
	);
	const scriptPath = fileURLToPath(import.meta.url);
	const execArgv = process.execArgv.filter((argument) => !argument.startsWith("--inspect"));
	if (!execArgv.includes("--expose-gc")) execArgv.push("--expose-gc");
	const child = spawn(
		process.execPath,
		[
			...execArgv,
			scriptPath,
			"--worker",
			"--arm",
			arm,
			"--runtime-root",
			runtimeRoot,
			"--fixture",
			fixture,
			"--width",
			String(width),
		],
		{
			cwd: sampleRoot,
			env: sanitizedWorkerEnvironment(sampleRoot),
			stdio: ["ignore", "pipe", "pipe"],
		},
	);
	let stdout = "";
	let stderr = "";
	child.stdout.setEncoding("utf8");
	child.stderr.setEncoding("utf8");
	child.stdout.on("data", (chunk: string) => {
		stdout = (stdout + chunk).slice(-1024 * 1024);
	});
	child.stderr.on("data", (chunk: string) => {
		stderr = (stderr + chunk).slice(-1024 * 1024);
	});
	const [exitCode, signal] = (await once(child, "close")) as [number | null, NodeJS.Signals | null];
	if (exitCode !== 0) {
		throw new Error(`History ${arm} sample failed (exit=${exitCode}, signal=${signal}): ${stderr || stdout}`);
	}
	const line = stdout
		.split(/\r?\n/)
		.reverse()
		.find((candidate) => candidate.startsWith(SAMPLE_PREFIX));
	if (!line) throw new Error(`History ${arm} sample did not return structured output`);
	return JSON.parse(line.slice(SAMPLE_PREFIX.length)) as HistorySample;
}

function distribution(values: number[]): NumericDistribution {
	if (values.length === 0 || values.some((value) => !Number.isFinite(value))) {
		throw new Error("Cannot summarize an empty or non-finite sample set");
	}
	const sorted = [...values].sort((left, right) => left - right);
	const middle = Math.floor(sorted.length / 2);
	const median =
		sorted.length % 2 === 0 ? (sorted[middle - 1]! + sorted[middle]!) / 2 : sorted[middle]!;
	return { samples: sorted.length, median, min: sorted[0]!, max: sorted.at(-1)! };
}

function nullableDistribution(values: Array<number | null>, unavailableReason: string): NullableDistribution {
	const available = values.filter((value): value is number => value !== null && Number.isFinite(value));
	return {
		availableSamples: available.length,
		distribution: available.length > 0 ? distribution(available) : null,
		...(available.length === values.length ? {} : { unavailableReason }),
	};
}

function summarize(samples: HistorySample[]) {
	return {
		sampleCount: samples.length,
		primaryFullFileParseMs: distribution(samples.map((sample) => sample.primaryFullFileParseMs)),
		managerHydrationMs: distribution(samples.map((sample) => sample.managerHydrationMs)),
		snapshotBuildMs: distribution(samples.map((sample) => sample.snapshotBuildMs)),
		serializationMs: distribution(samples.map((sample) => sample.serializationMs)),
		transportMs: distribution(samples.map((sample) => sample.transportMs)),
		wireBytes: distribution(samples.map((sample) => sample.wireBytes)),
		firstUsableRenderMs: distribution(samples.map((sample) => sample.firstUsableRenderMs)),
		processMaxRssBytes: nullableDistribution(
			samples.map((sample) => sample.processMaxRssBytes),
			"Node process.resourceUsage().maxRSS was unavailable; boundary RSS is reported separately and is not relabelled as a true peak.",
		),
		boundaryMaxRssBytes: distribution(samples.map((sample) => sample.boundaryMaxRssBytes)),
		boundaryMaxHeapUsedBytes: distribution(samples.map((sample) => sample.boundaryMaxHeapUsedBytes)),
		primaryReadBytes: nullableDistribution(
			samples.map((sample) => sample.primaryReadBytes),
			"This runtime does not expose bytes from the actual primary transcript read; fixture size remains separate.",
		),
		lifetimeMessageCount: distribution(samples.map((sample) => sample.lifetimeMessageCount)),
		visibleMessageCount: distribution(samples.map((sample) => sample.visibleMessageCount)),
		renderedLineCount: distribution(samples.map((sample) => sample.renderedLineCount)),
	};
}

async function orchestrate(options: ComparatorOptions): Promise<void> {
	const manifest = await validateSyntheticFixture(options.fixture, options.manifest);
	const fixtureSha256 = await sha256(options.fixture);
	for (const [label, root] of [
		["baseline", options.baselineRoot],
		["combined", options.combinedRoot],
	] as const) {
		if (!existsSync(join(root, "package.json"))) throw new Error(`${label} coding-agent root is missing package.json`);
		runtimeModulePath(root, "core/session-manager.js");
		runtimeModulePath(root, "modes/interactive/interactive-mode.js");
	}
	let ownedWorkRoot: string;
	if (options.workRoot) {
		await mkdir(options.workRoot, { recursive: true });
		ownedWorkRoot = await mkdtemp(join(options.workRoot, "history-compare-"));
	} else {
		ownedWorkRoot = await mkdtemp(join(tmpdir(), "prime-history-compare-"));
	}
	const measured: Record<Arm, HistorySample[]> = { baseline: [], combined: [] };
	try {
		for (let warmup = 0; warmup < options.warmups; warmup++) {
			for (const arm of ["baseline", "combined"] as const) {
				const sampleRoot = join(ownedWorkRoot, `warmup-${warmup}-${arm}`);
				await mkdir(sampleRoot, { recursive: true });
				const copy = join(sampleRoot, "synthetic-history.jsonl");
				await copyFile(options.fixture, copy);
				await runChildSample(
					arm,
					arm === "baseline" ? options.baselineRoot : options.combinedRoot,
					copy,
					options.width,
					sampleRoot,
				);
			}
		}
		for (let trial = 0; trial < options.trials; trial++) {
			const order: Arm[] = trial % 2 === 0 ? ["baseline", "combined"] : ["combined", "baseline"];
			for (const arm of order) {
				const sampleRoot = join(ownedWorkRoot, `measured-${trial}-${arm}`);
				await mkdir(sampleRoot, { recursive: true });
				const copy = join(sampleRoot, "synthetic-history.jsonl");
				await copyFile(options.fixture, copy);
				const sample = await runChildSample(
					arm,
					arm === "baseline" ? options.baselineRoot : options.combinedRoot,
					copy,
					options.width,
					sampleRoot,
				);
				if (sample.fixtureBytes !== manifest.bytes) throw new Error("Measured fixture copy changed size");
				measured[arm].push(sample);
			}
		}
		const report = {
			schemaVersion: 1,
			comparison: "unchanged-baseline-vs-combined-history",
			baseline: { sourceId: options.baselineId, runtimeRoot: options.baselineRoot, summary: summarize(measured.baseline) },
			combined: { sourceId: options.combinedId, runtimeRoot: options.combinedRoot, summary: summarize(measured.combined) },
			fixture: {
				path: options.fixture,
				manifest: options.manifest,
				sha256: fixtureSha256,
				bytes: manifest.bytes,
				mib: manifest.mib,
				messageCount: manifest.messageCount,
				toolCycles: manifest.toolCycles,
				syntheticOnly: true,
				productionData: false,
			},
			method: {
				warmupsPerArm: options.warmups,
				measuredSamplesPerArm: options.trials,
				order: "alternating baseline-first/combined-first",
				freshProcessPerSample: true,
				freshByteIdenticalFixtureCopyPerSample: true,
				historyWindowMessages: HISTORY_WINDOW_MESSAGES,
				renderWidth: options.width,
				firstUsableRenderDefinition:
					"Completion of the runtime's existing InteractiveMode.renderSessionContext followed by its real Container.render(width).",
				terminalFirstPaintMs: null,
				terminalFirstPaintUnavailableReason:
					"The existing TUI interface exposes requestRender but no terminal-flush completion timestamp; no terminal paint latency is invented.",
				transportDefinition:
					"Bytes from the runtime's existing JSONL serializer, counted at an isolated OS loopback receiver; connect setup is excluded.",
				parseDefinition:
					"The runtime's exported loadEntriesFromFileAsync full-file loader; manager hydration is timed separately and the bounded repair preflight is excluded.",
			},
			notes: [
				"This is a combined-candidate comparison and does not attribute results to one fix.",
				"Primary full-file parse, manager hydration, snapshot construction, JSON serialization, transport, and UI component rendering are timed separately.",
				"Ranges are median/min/max only; no tiny-n p95 is reported.",
				"The baseline may not expose actual primary-read bytes; that field remains unavailable rather than using stat-after-open as observed IO.",
				"No model/provider request is made and no token, quality, cost, or billing claim is supported.",
			],
			rawSamples: measured,
		};
		const serialized = `${JSON.stringify(report, null, 2)}\n`;
		if (options.output) {
			await mkdir(dirname(options.output), { recursive: true });
			await writeFile(options.output, serialized, "utf8");
		} else process.stdout.write(serialized);
	} finally {
		if (!options.keepWork) await rm(ownedWorkRoot, { recursive: true, force: true });
	}
}

async function main(): Promise<void> {
	const argv = process.argv.slice(2);
	if (argv[0] === "--worker") {
		const sample = await runWorker(parseWorkerOptions(argv.slice(1)));
		process.stdout.write(`${SAMPLE_PREFIX}${JSON.stringify(sample)}\n`);
		return;
	}
	await orchestrate(parseComparatorOptions(argv));
}

await main();
