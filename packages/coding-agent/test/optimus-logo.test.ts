import stripAnsi from "strip-ansi";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { initTheme, Theme } from "../src/modes/interactive/theme/theme.js";
import { colorizeOptimusLogo, getOptimusLogo, OPTIMUS_ROBOT_LOGO } from "../src/themes/optimus-logo.js";
import { OPTIMUS_ART } from "../src/themes/optimus-logo-data.js";

describe("Optimus robot artwork", () => {
	beforeEach(() => {
		vi.stubEnv("NO_COLOR", "");
		initTheme("dark");
	});

	afterEach(() => {
		vi.restoreAllMocks();
		vi.unstubAllEnvs();
	});

	it("preserves the supplied artwork at its original size", () => {
		expect(getOptimusLogo(50, 25)).toEqual(OPTIMUS_ART.hero);
		expect(OPTIMUS_ART.hero).toHaveLength(25);
		expect(Math.max(...OPTIMUS_ART.hero.map((line) => line.length))).toBe(50);
		expect(getOptimusLogo(100, 49).join("\n")).toBe(OPTIMUS_ROBOT_LOGO);
		expect(getOptimusLogo(200, 100).join("\n")).toBe(OPTIMUS_ROBOT_LOGO);
	});

	it.each([
		[20, 10],
		[32, 16],
		[49, 24],
		[60, 31],
		[1, 1],
	])("fits the full silhouette into %i columns and %i rows", (width, rows) => {
		const lines = getOptimusLogo(width, rows);
		expect(lines.length).toBeLessThanOrEqual(rows);
		expect(lines[0].trim()).not.toBe("");
		expect(lines.at(-1)?.trim()).not.toBe("");
		for (const line of lines) {
			expect(line.length).toBeLessThanOrEqual(width);
			expect(line).toMatch(/^[ *\u2800-\u28ff]*$/);
		}
	});

	it.each(["truecolor", "256color"] as const)(
		"uses %s portrait colors without changing text or leaking color",
		(mode) => {
			vi.spyOn(Theme.prototype, "getColorMode").mockReturnValue(mode);
			const raw = " .:-=+*#%@";
			const colored = colorizeOptimusLogo(raw);
			expect(stripAnsi(colored)).toBe(raw);
			expect(colored).toContain(mode === "truecolor" ? "\x1b[38;2;" : "\x1b[38;5;");
			expect(colored.endsWith("\x1b[39m")).toBe(true);
			initTheme("light");
			expect(colorizeOptimusLogo(raw)).not.toBe(colored);
		},
	);

	it("preserves every approved colored dot and does not expose cached arrays for mutation", () => {
		vi.spyOn(Theme.prototype, "getColorMode").mockReturnValue("truecolor");
		for (const variant of OPTIMUS_ART.variants) {
			variant.lines.forEach((line, row) => {
				let tone = -1;
				let expected = "";
				Array.from(line).forEach((char, col) => {
					if (char !== " ") {
						const next = variant.tones[row][col];
						if (next !== tone) {
							const hex = OPTIMUS_ART.palettes.dark[next];
							expected += `\x1b[38;2;${Number.parseInt(hex.slice(1, 3), 16)};${Number.parseInt(hex.slice(3, 5), 16)};${Number.parseInt(hex.slice(5, 7), 16)}m`;
						}
						tone = next;
					}
					expected += char;
				});
				expect(colorizeOptimusLogo(line)).toBe(expected + (tone < 0 ? "" : "\x1b[39m"));
			});
		}
		const first = getOptimusLogo(50, 25) as string[];
		first[0] = "mutated";
		expect(getOptimusLogo(50, 25)).toEqual(OPTIMUS_ART.hero);
	});

	it("honors NO_COLOR for the artwork and wordmark", () => {
		vi.stubEnv("NO_COLOR", "1");
		expect(colorizeOptimusLogo(OPTIMUS_ROBOT_LOGO)).toBe(OPTIMUS_ROBOT_LOGO);
		expect(colorizeOptimusLogo("OPTIMUS")).toBe("OPTIMUS");
	});
});
