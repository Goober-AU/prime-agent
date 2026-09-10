import { rgbTo256 } from "@earendil-works/pi-tui";
import { theme } from "../modes/interactive/theme/theme.js";
import { OPTIMUS_ART } from "./optimus-logo-data.js";

export const OPTIMUS_ROBOT_LOGO = OPTIMUS_ART.hero.join("\n");
const tonesByRow = new Map<string, readonly number[]>();
for (const variant of OPTIMUS_ART.variants) {
	variant.lines.forEach((line, row) => {
		if (!tonesByRow.has(line)) tonesByRow.set(line, variant.tones[row]);
	});
}

/** Select a complete portrait instead of averaging its edges into dense shading. */
export function getOptimusLogo(maxWidth: number, maxRows: number): readonly string[] {
	const width = Number.isFinite(maxWidth) ? Math.max(1, Math.floor(maxWidth)) : 50;
	const rows = Number.isFinite(maxRows) ? Math.max(1, Math.floor(maxRows)) : 25;
	const selected = OPTIMUS_ART.variants.find(
		(variant) => variant.lines.length <= rows && variant.lines.every((line) => line.length <= width),
	);
	return [...(selected?.lines ?? ["*"])];
}

export function colorizeOptimusLogo(text: string): string {
	if (process.env.NO_COLOR) return text;
	const palette = OPTIMUS_ART.palettes[theme.name === "light" ? "light" : "dark"];
	const colors = palette.map((hex) => {
		const rgb = {
			r: Number.parseInt(hex.slice(1, 3), 16),
			g: Number.parseInt(hex.slice(3, 5), 16),
			b: Number.parseInt(hex.slice(5, 7), 16),
		};
		return theme.getColorMode() === "truecolor"
			? `\x1b[38;2;${rgb.r};${rgb.g};${rgb.b}m`
			: `\x1b[38;5;${rgbTo256(rgb)}m`;
	});
	return text
		.split("\n")
		.map((line) => {
			const tones = tonesByRow.get(line);
			let output = "",
				previous = -1;
			Array.from(line).forEach((char, index) => {
				if (char !== " ") {
					const tone = tones && tones[index] >= 0 ? tones[index] : 3;
					if (tone !== previous) output += colors[tone];
					previous = tone;
				}
				output += char;
			});
			return previous < 0 ? output : `${output}\x1b[39m`;
		})
		.join("\n");
}
