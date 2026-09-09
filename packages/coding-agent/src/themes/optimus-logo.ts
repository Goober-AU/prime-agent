import { rgbTo256 } from "@earendil-works/pi-tui";
import { theme } from "../modes/interactive/theme/theme.js";

/** Original artwork from neon_green_robot_ascii.txt, preserved at 100 columns × 49 rows. */
export const OPTIMUS_ROBOT_LOGO = `                                          #**######%%%@           ###%
                                    #*+++++*****##%%@@@@%#%%%%*******@
                                 #======+****%%@@@@@%#%%%%%*+++****##
                               #*--++==*****@#+===++*%@@#*++**+*#*##%
                             ##+:=+=-=****##*======+*##**#%#*=-*###%
                            #*+:-===+***#**++**#%#*##**#%%%*+-*#%@@
                          %*+#=+#%%#*#**++***#%%%%#*+*%%%%%#+*%%@@%#%
                         @**-#@@%#***++**####%%%#@**#%%%%%@%**#%%%@*#
                         ##+==##*****#######%%%%-%%#%%%%@@%**#+==*##%
                        #=%==:-+*#*########%%%%%+%%##%%@@%#%#:.=*####%@
                       @++@+:.+*##**######%%%%%%%@@%%%@@*-*%=-+%#==++#@
                       #=*%*==*###**#####%%%%%%%@@@@@%##..#%#=%%+=%%#+%@
                       #*@%+-=*###**####%%##*=-=*%%%@*+#--%%#=%%-*#***#@
                       @%%%*=-+##*######+=-::-+##%%%%#*#==#%#+%%+=%%%+#@
                         @%##==##%%%%+-:-=+*##%%%*=+*%#%++*@#*#@%=+*++%@
                          @#==+%@@@@#=+*#*****#%==*%%%%%%#*#%###%%#**#%
                          @+**#**#%%%%***+***#@*+*%@##%@@%#*#%%######%
                           +=*#**#%%%*+****#%%@++-%%#%%%%@@%##%%%%%%
                           @#*****%#+*****#@%%%=++@#%%%#%@@@@@@@@@%%
                            %##*+==+****+#@%%@*+####%%%#@@@@@@%@@@%%
                             #%#*-:+***#*%@%%@+*##%%%%%@@@@%%%#@@@@@
                             %*%%**###*#%@%%%%*#%%%%@@@@@@%%%%#@%@@%%##%       #**#****#
                              %#%#+=+%%%%%%%%##%%@@@@%*%%@@@%%#@%@@@@%#+%     #**++##*****##%
                               %@#==+*#%#%%%@#%@@@@@%+-%%@%%%%%%%##****+*%#+*#****#==++++++*****#%
                                 %+++**%%%@@@@@@@@@%#:#@@##%%%*==*#*****####*****#-:===+++********#@
                                  %%%%%%@@@%%@@%@@%%=+%@%%%#+=-=**###%%##%%#**+#*::--=====+#%%%%%%%@
                               ##**%@@##%%@%%%%%@@%%#%%@@*=-::+###%%%%%%@@*++*#+.:::---===+#********
                            %###+*%@@@##+%@##%%@@#*%%%@@*+++=*%%######%%@*+*###=----========++++****
             #**+++*#%%@@@@###*=#@@@@%%%*%**#%@%**%%%%##+***#@@###*++#@@#*##+:*@%*+=-======---++****
          **++++**%@@@@@%%##%+-%@@@@@##@@#*#%@%**#######**####%%@%##%@@#+==-::+@%%##+=----::.:=*****
       %*+=+#***%@@@@@###*++*#+%@%#%####*#%%@@%#*++****#######%%%%%%%@=:--====+@%#++*#*=:...-==+****
      *+*++#***#%@@@@%##==+***#%#*******#%%%@%*==*##########%%%%#%@@@#:========%%%%#+=*#*-:=====+***
    %+==+#%**+*#@@@%*++=+****+#+-====--=*%%%#*+*#######%%%%#%%#*#%@%@+-========*##%%%#*+#@#======+#%
   %#+-..-%***#%@%*==+*+**#+**++******++++%%**#####%%%%%%%#%%#*****#@@%*+====+++++*##%%%%%@#==++++##
  %##%*::-#%#%%%*=+++*@*#*+*+*#%**++*##+===%#*###%%%##***#%%******++%%%@@%*+++++++++++##%%%@*++++###
 @*==-%=++*@%%*##*++++%@*+*+*#+-....:**#+--=%%#******####********++%@%%@@@%%%*++++++--***#%%@*++*###
@#=*%=**+**@#**#=#+++==#@*+##+:......:*##+*+#@#*####**++=+********%@%%@@@*-%%%%#**+=.=******%@*+####
@++%%=****#*****+=#++==+@+*%*-.......=*#*##%@#***++++++++***###%%@%%%@@@@:*##@##@@%#*##******%@**###
%=*%%-#++#%*****#==*=+**%#+#*+.....:+***###%#=-=+++++++*##%%%##%%###%@@@%=#%%@##%@@##%@@%%#***%%*###
%=*%++#*#@@%#****#+#****#%=*%*+----******%%*=-==+++**##%%#*+=+*###%%##%@@*+%%%@%%@@%+*%@@@@%***%@@@@
@#==+%**%%#@%%##**#****#%@++%%######****%%*=-=++*######+==+*##%%%#+--+*%@@**%%%@%%@%++%@@%@@@#***###
 @##%###%#-*@%%%%#***#%%%@#+++++++++**#%****#%#######%#*###%%#*+==+*###*%@@#*#%@@@%###@@%@##%@#**###
  %##%%#%%+==%@%%%%##*%%%%%+=+*****+*#%***#%%%%#####%##%%%@*==+**######*#@@@@%%%###%%@@@@%###%@%#%%%
   %##%%@@**=#@%%%%%%##%%%%#*#######%%*##%%%%%#%%##%%%%%@@*+****#######%%@@@@@@@@@@@%#*#%%%####%@@@%
   ##%@@@@#**#@%%%@%%%%%%%@@#**###%@@%#%%####%%%@@@@@@%@%++***#####%%%%%%%%@@@@@%*+=--+***#%###%@@@#
  %****#%%@%**%@%%@%%%%%%@%+*****##%@@%#%%%%@@%@@@%@@@@@#+***###%%%%%%%%%%@@@@%+-====++****#%#%@@@*+
 @##*###%%%@@##%@%@%@#%%@%**#####%%#%@@@%%%#*#%%%%%%@%%%@%*##%@@@%%%%%@@@@@@@#**====++++**#*%@@@%++*
 %#**##%%%@@@@@%@@%%@#=+@***#####%%%@@@+++**#@%%%%@%%%%%%@@@@@@%@%%@@@@@@@@@@***+=++++++*##**@@@*+**
@##*###%%%@%%@@@%%@%%@#+#%***###%%%@@@*+####%%%%@%%%%%%%@@%#%@%@@%@@@@@@@@@@@%***++++++******#@@****`;

const DENSITY_RAMP = " .:-=+*#%@";
const SOURCE_LINES = OPTIMUS_ROBOT_LOGO.split("\n");
const SOURCE_WIDTH = Math.max(...SOURCE_LINES.map((line) => line.length));
let cachedSize = "";
let cachedLines: readonly string[] = [];

/** Fit the entire robot into a terminal rectangle without cropping or changing its proportions. */
export function getOptimusLogo(maxWidth: number, maxRows: number): readonly string[] {
	const widthLimit = Number.isFinite(maxWidth) ? Math.max(1, Math.floor(maxWidth)) : SOURCE_WIDTH;
	const rowLimit = Number.isFinite(maxRows) ? Math.max(1, Math.floor(maxRows)) : SOURCE_LINES.length;
	const scale = Math.min(1, widthLimit / SOURCE_WIDTH, rowLimit / SOURCE_LINES.length);
	const width = Math.max(1, Math.floor(SOURCE_WIDTH * scale));
	const rows = Math.max(1, Math.floor(SOURCE_LINES.length * scale));
	const size = `${width}x${rows}`;
	if (size === cachedSize) return cachedLines;
	if (width === SOURCE_WIDTH && rows === SOURCE_LINES.length) return SOURCE_LINES;

	// Area averaging preserves the source's density shading when several ASCII cells become one.
	const lines: string[] = [];
	for (let y = 0; y < rows; y++) {
		let line = "";
		const top = (y * SOURCE_LINES.length) / rows;
		const bottom = ((y + 1) * SOURCE_LINES.length) / rows;
		for (let x = 0; x < width; x++) {
			const left = (x * SOURCE_WIDTH) / width;
			const right = ((x + 1) * SOURCE_WIDTH) / width;
			let density = 0;
			for (let sy = Math.floor(top); sy < Math.ceil(bottom); sy++) {
				for (let sx = Math.floor(left); sx < Math.ceil(right); sx++) {
					const area =
						(Math.min(right, sx + 1) - Math.max(left, sx)) * (Math.min(bottom, sy + 1) - Math.max(top, sy));
					density += DENSITY_RAMP.indexOf(SOURCE_LINES[sy]?.[sx] ?? " ") * area;
				}
			}
			line += DENSITY_RAMP[Math.round(density / ((right - left) * (bottom - top)))];
		}
		lines.push(line.trimEnd());
	}
	cachedSize = size;
	cachedLines = lines;
	return lines;
}

/** Neon-green shadows and highlights, with darker ink for the light theme. */
export function colorizeOptimusLogo(text: string): string {
	if (process.env.NO_COLOR) return text;
	const palette =
		theme.name === "light"
			? [
					{ r: 80, g: 132, b: 62 },
					{ r: 43, g: 116, b: 25 },
					{ r: 22, g: 88, b: 10 },
				]
			: [
					{ r: 40, g: 140, b: 48 },
					{ r: 72, g: 216, b: 55 },
					{ r: 140, g: 255, b: 90 },
				];
	const colors = palette.map((rgb) =>
		theme.getColorMode() === "truecolor" ? `\x1b[38;2;${rgb.r};${rgb.g};${rgb.b}m` : `\x1b[38;5;${rgbTo256(rgb)}m`,
	);
	let output = "";
	let previousTone = -1;
	for (const char of text) {
		if (char !== " ") {
			const density = DENSITY_RAMP.indexOf(char);
			const tone = density < 0 || density >= 7 ? 2 : density >= 4 ? 1 : 0;
			if (tone !== previousTone) output += colors[tone];
			previousTone = tone;
		}
		output += char;
	}
	return previousTone < 0 ? output : `${output}\x1b[39m`;
}
