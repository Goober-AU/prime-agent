import { describe, expect, it } from "vitest";
import { isUnsafeWindowsCapturedLauncher } from "../src/core/tools/ipython.js";

describe("Windows persistent launcher capture guard", () => {
	it.each(["start", "serve", "launch", "run"])("rejects captured %s.ps1", (name) => {
		expect(
			isUnsafeWindowsCapturedLauncher(
				`subprocess.run(['pwsh', '${name}.ps1'], capture_output=True, timeout=30)`,
				"win32",
			),
		).toBe(true);
	});
	it.each([
		"subprocess.run(['pwsh', 'run-tests.ps1'], capture_output=True, timeout=30)",
		"subprocess.run(['pwsh', 'start.ps1'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)",
		"subprocess.run(['pwsh', 'serve.ps1'], stdout=log, stderr=log, timeout=30)",
		"await bash('echo ok')",
	])("preserves bounded commands or explicit redirection: %s", (code) => {
		expect(isUnsafeWindowsCapturedLauncher(code, "win32")).toBe(false);
	});
	it("does not change non-Windows behavior", () => {
		expect(
			isUnsafeWindowsCapturedLauncher("subprocess.run(['pwsh', 'start.ps1'], capture_output=True)", "linux"),
		).toBe(false);
	});
});
