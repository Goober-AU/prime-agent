import { getPiUserAgent } from "./pi-user-agent.js";
import { PRIME_AGENT_UPDATE_RELEASE_URL, PRIME_AGENT_UPDATE_REPOSITORY_URL } from "./update-source.js";

const DEFAULT_PRIME_AGENT_DOWNLOAD_BASE_URL = `${PRIME_AGENT_UPDATE_RELEASE_URL}/download`;
const MAIN_VERSION_MANIFEST_PATH = "main.json";
const DEFAULT_VERSION_CHECK_TIMEOUT_MS = 10000;

export interface LatestPiRelease {
	version: string;
	packageName: string;
	installSpec: string;
}

interface ParsedVersion {
	major: number;
	minor: number;
	patch: number;
	prerelease?: string;
}

function comparePrereleaseIdentifiers(leftPrerelease: string, rightPrerelease: string): number {
	const leftIdentifiers = leftPrerelease.split(".");
	const rightIdentifiers = rightPrerelease.split(".");
	const length = Math.max(leftIdentifiers.length, rightIdentifiers.length);

	for (let index = 0; index < length; index += 1) {
		const left = leftIdentifiers[index];
		const right = rightIdentifiers[index];
		if (left === right) continue;
		if (left === undefined) return -1;
		if (right === undefined) return 1;

		const leftIsNumeric = /^\d+$/.test(left);
		const rightIsNumeric = /^\d+$/.test(right);
		if (leftIsNumeric && rightIsNumeric) {
			const leftNumber = left.replace(/^0+(?=\d)/, "");
			const rightNumber = right.replace(/^0+(?=\d)/, "");
			if (leftNumber.length !== rightNumber.length) return leftNumber.length - rightNumber.length;
			const comparison = leftNumber.localeCompare(rightNumber);
			if (comparison !== 0) return comparison;
			continue;
		}
		if (leftIsNumeric) return -1;
		if (rightIsNumeric) return 1;
		return left.localeCompare(right);
	}

	return 0;
}

function parsePackageVersion(version: string): ParsedVersion | undefined {
	const match = version.trim().match(/^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+.*)?$/);
	if (!match) {
		return undefined;
	}
	return {
		major: Number.parseInt(match[1], 10),
		minor: Number.parseInt(match[2], 10),
		patch: Number.parseInt(match[3], 10),
		prerelease: match[4],
	};
}

export function comparePackageVersions(leftVersion: string, rightVersion: string): number | undefined {
	const left = parsePackageVersion(leftVersion);
	const right = parsePackageVersion(rightVersion);
	if (!left || !right) {
		return undefined;
	}

	if (left.major !== right.major) return left.major - right.major;
	if (left.minor !== right.minor) return left.minor - right.minor;
	if (left.patch !== right.patch) return left.patch - right.patch;
	if (left.prerelease === right.prerelease) return 0;
	if (!left.prerelease) return 1;
	if (!right.prerelease) return -1;
	return comparePrereleaseIdentifiers(left.prerelease, right.prerelease);
}

export function isNewerPackageVersion(candidateVersion: string, currentVersion: string): boolean {
	const comparison = comparePackageVersions(candidateVersion, currentVersion);
	if (comparison !== undefined) {
		return comparison > 0;
	}
	return candidateVersion.trim() !== currentVersion.trim();
}

export function isMainBuildUpdateAvailable(candidateVersion: string, currentVersion: string): boolean {
	const current = parsePackageVersion(currentVersion);
	if (current?.prerelease?.startsWith("telemusai.main.")) {
		return isNewerPackageVersion(candidateVersion, currentVersion);
	}
	return normalizeReleaseVersion(candidateVersion) !== normalizeReleaseVersion(currentVersion);
}

function getPrimeAgentDownloadBaseUrl(): string {
	return (process.env.PRIME_AGENT_DOWNLOAD_BASE_URL?.trim() || DEFAULT_PRIME_AGENT_DOWNLOAD_BASE_URL).replace(
		/\/+$/,
		"",
	);
}

function normalizeReleaseVersion(version: string): string {
	return version.trim().replace(/^v/, "");
}

function resolveReleaseUrl(baseUrl: string, pathOrUrl: string): string | undefined {
	const trimmed = pathOrUrl.trim();
	if (!trimmed) return undefined;
	try {
		const url = new URL(trimmed, `${baseUrl}/`);
		return url.protocol === "https:" || url.protocol === "http:" ? url.toString() : undefined;
	} catch {
		return undefined;
	}
}

export async function getLatestPiRelease(
	currentVersion: string,
	options: { timeoutMs?: number } = {},
): Promise<LatestPiRelease | undefined> {
	if (process.env.PI_SKIP_VERSION_CHECK || process.env.PI_OFFLINE) return undefined;

	const baseUrl = getPrimeAgentDownloadBaseUrl();
	const response = await fetch(`${baseUrl}/${MAIN_VERSION_MANIFEST_PATH}`, {
		headers: {
			"User-Agent": getPiUserAgent(currentVersion),
			accept: "application/json",
		},
		signal: AbortSignal.timeout(options.timeoutMs ?? DEFAULT_VERSION_CHECK_TIMEOUT_MS),
	});
	if (!response.ok) return undefined;

	const data: unknown = await response.json();
	if (typeof data !== "object" || data === null || Array.isArray(data)) return undefined;
	if (
		!("channel" in data) ||
		data.channel !== "main" ||
		!("version" in data) ||
		typeof data.version !== "string" ||
		!parsePackageVersion(data.version) ||
		!("package" in data) ||
		data.package !== "prime-agent" ||
		!("tarball" in data) ||
		typeof data.tarball !== "string"
	) {
		return undefined;
	}
	const version = normalizeReleaseVersion(data.version);
	const installSpec = resolveReleaseUrl(baseUrl, data.tarball);
	if (!installSpec || !new URL(installSpec).pathname.endsWith(`/prime-agent-${version}.tgz`)) return undefined;
	if (
		!process.env.PRIME_AGENT_DOWNLOAD_BASE_URL?.trim() &&
		installSpec !== `${PRIME_AGENT_UPDATE_REPOSITORY_URL}/releases/download/v${version}/prime-agent-${version}.tgz`
	)
		return undefined;
	return { version, packageName: data.package, installSpec };
}

export async function getLatestPiVersion(
	currentVersion: string,
	options: { timeoutMs?: number } = {},
): Promise<string | undefined> {
	return (await getLatestPiRelease(currentVersion, options))?.version;
}

export async function checkForNewPiVersion(currentVersion: string): Promise<string | undefined> {
	try {
		const latestVersion = await getLatestPiVersion(currentVersion);
		// A main build may have a lower semver than an installed stable or custom build.
		if (latestVersion && isMainBuildUpdateAvailable(latestVersion, currentVersion)) {
			return latestVersion;
		}
		return undefined;
	} catch {
		return undefined;
	}
}
