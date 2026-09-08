import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
	checkForNewPiVersion,
	comparePackageVersions,
	getLatestPiRelease,
	getLatestPiVersion,
	isNewerPackageVersion,
} from "../src/utils/version-check.js";

const repositoryUrl = "https://github.com/telemusai/prime-agent";
const mainManifestUrl = `${repositoryUrl}/releases/latest/download/main.json`;

function mainRelease(version = "1.2.4-telemusai.main.10.abcdef012") {
	return {
		channel: "main",
		version: `v${version}`,
		package: "prime-agent",
		tarball: `${repositoryUrl}/releases/download/v${version}/prime-agent-${version}.tgz`,
	};
}

beforeEach(() => {
	vi.stubEnv("PI_SKIP_VERSION_CHECK", undefined);
	vi.stubEnv("PI_OFFLINE", undefined);
	vi.stubEnv("PRIME_AGENT_DOWNLOAD_BASE_URL", undefined);
});

afterEach(() => {
	vi.unstubAllGlobals();
	vi.unstubAllEnvs();
});

describe("version checks", () => {
	it("compares package versions", () => {
		expect(comparePackageVersions("0.70.6", "0.70.5")).toBeGreaterThan(0);
		expect(comparePackageVersions("0.70.5", "0.70.5")).toBe(0);
		expect(comparePackageVersions("0.70.4", "0.70.5")).toBeLessThan(0);
		expect(comparePackageVersions("0.70.5-beta.10.1.abcdef0", "0.70.5-beta.9.1.1234567")).toBeGreaterThan(0);
		expect(isNewerPackageVersion("0.70.5", "0.70.5")).toBe(false);
		expect(isNewerPackageVersion("0.70.6", "0.70.5")).toBe(true);
	});

	it.each(["1.2.3", "1.2.3-beta.123.1.1234567", "0.9.3-telemusai.astra.c971060ea"])(
		"uses the fork main manifest when upgrading %s",
		async (currentVersion) => {
			const fetchMock = vi.fn(async () => Response.json(mainRelease()));
			vi.stubGlobal("fetch", fetchMock);
			await expect(getLatestPiVersion(currentVersion)).resolves.toBe("1.2.4-telemusai.main.10.abcdef012");
			expect(fetchMock).toHaveBeenCalledExactlyOnceWith(
				mainManifestUrl,
				expect.objectContaining({
					headers: expect.objectContaining({
						"User-Agent": expect.stringContaining(`prime-agent/${currentVersion} `),
						accept: "application/json",
					}),
				}),
			);
		},
	);

	it("moves stable installs to main even when its prerelease sorts lower", async () => {
		vi.stubGlobal(
			"fetch",
			vi.fn(async () => Response.json(mainRelease())),
		);
		await expect(checkForNewPiVersion("1.2.4")).resolves.toBe("1.2.4-telemusai.main.10.abcdef012");
		await expect(checkForNewPiVersion("1.2.4-telemusai.main.10.abcdef012")).resolves.toBeUndefined();
	});

	it("returns the immutable fork tarball for the discovered version", async () => {
		const release = mainRelease();
		vi.stubGlobal(
			"fetch",
			vi.fn(async () => Response.json(release)),
		);
		await expect(getLatestPiRelease("1.2.3")).resolves.toEqual({
			installSpec: release.tarball,
			packageName: "prime-agent",
			version: release.version.slice(1),
		});
	});

	it("does not downgrade an installed main build when a cached manifest is older", async () => {
		vi.stubGlobal(
			"fetch",
			vi.fn(async () => Response.json(mainRelease())),
		);
		await expect(checkForNewPiVersion("1.2.4-telemusai.main.11.123456789")).resolves.toBeUndefined();
		await expect(checkForNewPiVersion("1.2.4-telemusai.main.9.123456789")).resolves.toBe(
			"1.2.4-telemusai.main.10.abcdef012",
		);
	});

	it.each([
		null,
		[],
		{ version: "1.2.4" },
		{ ...mainRelease(), channel: "stable" },
		{ ...mainRelease(), channel: "beta" },
		{ ...mainRelease(), version: "invalid" },
		{ ...mainRelease(), package: "another-package" },
		{ ...mainRelease(), tarball: "" },
		{ ...mainRelease(), tarball: "file:///tmp/prime-agent.tgz" },
		{ ...mainRelease(), tarball: mainRelease().tarball.replace("telemusai/", "PrimeIntellect-ai/") },
		{ ...mainRelease(), tarball: mainRelease().tarball.replace(".tgz", ".zip") },
	])("rejects a manifest that cannot install the fork main build: %j", async (manifest) => {
		vi.stubGlobal(
			"fetch",
			vi.fn(async () => Response.json(manifest)),
		);
		await expect(getLatestPiRelease("1.2.3")).resolves.toBeUndefined();
	});

	it("allows an explicit mirror while retaining the main channel", async () => {
		vi.stubEnv("PRIME_AGENT_DOWNLOAD_BASE_URL", "http://127.0.0.1:18188/");
		const fetchMock = vi.fn(async () =>
			Response.json({
				...mainRelease("1.2.4"),
				tarball: "releases/v1.2.4/prime-agent-1.2.4.tgz",
			}),
		);
		vi.stubGlobal("fetch", fetchMock);
		await expect(getLatestPiRelease("1.2.3")).resolves.toEqual({
			version: "1.2.4",
			packageName: "prime-agent",
			installSpec: "http://127.0.0.1:18188/releases/v1.2.4/prime-agent-1.2.4.tgz",
		});
		expect(fetchMock).toHaveBeenCalledWith("http://127.0.0.1:18188/main.json", expect.any(Object));
	});

	it.each(["PI_SKIP_VERSION_CHECK", "PI_OFFLINE"])("does not fetch when %s is set", async (variable) => {
		vi.stubEnv(variable, "1");
		const fetchMock = vi.fn();
		vi.stubGlobal("fetch", fetchMock);
		await expect(getLatestPiVersion("1.2.3")).resolves.toBeUndefined();
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it("keeps startup quiet when GitHub is unavailable", async () => {
		const fetchMock = vi.fn(async () => {
			throw new Error("network unavailable");
		});
		vi.stubGlobal("fetch", fetchMock);
		await expect(checkForNewPiVersion("1.2.3")).resolves.toBeUndefined();
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});
});
