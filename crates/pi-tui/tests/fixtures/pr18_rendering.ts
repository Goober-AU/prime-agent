// Regenerate from the read-only TypeScript reference with:
// node --import tsx crates/pi-tui/tests/fixtures/pr18_rendering.ts
import { writeFileSync } from "node:fs";
import { Markdown } from "../../../../packages/tui/src/components/markdown.js";
import { latexToUnicode } from "../../../../packages/tui/src/latex.js";
import { stripAnsi, truncateToWidth, visibleWidth, wrapTextWithAnsi } from "../../../../packages/tui/src/utils.js";

const plain = (text: string) => text;
const theme = {
  heading: plain, link: plain, linkUrl: plain, code: plain, codeBlock: plain,
  codeBlockBorder: plain, quote: plain, quoteBorder: plain, hr: plain,
  listBullet: plain, bold: plain, italic: plain, strikethrough: plain, underline: plain,
};
const widthInputs = new Set<string>(["", "hello", "日本語", "กํา", "e\u0301", "👨‍👩‍👧‍👦", "☝️🏻", "🧑‍🧑‍🧒"]);
for (const [start, end] of [[0x2300, 0x27bf], [0x1f000, 0x1fbff]]) {
  for (let cp = start; cp <= end; cp++) {
    const ch = String.fromCodePoint(cp);
    for (const tail of ["", "\ufe0f", "\u0301", "🏻", "\ufe0f\u200d👩"]) widthInputs.add(ch + tail);
  }
}
for (let cp = 0x1f1e6; cp <= 0x1f1ff; cp++) {
  const ch = String.fromCodePoint(cp);
  for (const prefix of ["", "\u0600", "\u200d", "a", "\u0301"]) {
    widthInputs.add(prefix + ch);
    for (let next = 0x1f1e6; next <= 0x1f1ff; next++) widthInputs.add(prefix + ch + String.fromCodePoint(next));
  }
}
const latex = [
  String.raw`y_t = \sum_{k=0}^{W-1} w_k \odot x_{t-k}`,
  String.raw`\alpha \leq \beta \implies \gamma \to \infty`,
  String.raw`\frac{a}{b} + \frac{x+1}{2} + \frac{1}{2} m v^2`,
  String.raw`\sqrt{x} + \sqrt{x^2+1} + \sqrt[3]{8} + \sqrt[5]{x}`,
  String.raw`\text{learning_rate} = \mathrm{x_i}`,
  String.raw`\mathbb{R}^d + \mathcal{L} + \mathbf{W} x`,
  String.raw`\hat{y} + \vec{x} + \frac{\hat{x}}{2}`,
  String.raw`\nabla_\theta J(\theta) + x_{best}`,
  String.raw`\begin{aligned} a &= b + c \\ d &= e \end{aligned}`,
  String.raw`\foobar x + x \in \{1, \dots, K\}`,
  String.raw`\int_0^1 x^2 \, dx = \frac{1}{3}`,
  "a\n\n\nb", "a\ufeff\ufeffb", "a\u0085\u0085b", "a\n\ufeff\nb",
  "\\hat{a\ufeffb}", "\\hat{a\u0085b}", "x_\ufeff2", "x_\u00852",
];
const markdown = [
  "$$$$", "$$$$$", "$$ $$", "$$\n$$", "before $$$$ after", "$$$$\n\nafter",
  "$$x=2$$. Therefore y=3", "$$a=1$$ and $$b=2$$", "$$x$$$", "$$\ny_t = \\sum",
  "\\[\r\nE = mc^2\r\n\\]\r\n", "where \\(w_k\\) are shared", "value $x_i \\cdot y$ grows",
  "between $5 and $10 total", "prices $5,$10 listed", "a \\(x +\ny\\) b",
  "a \\(x +\n\ny\\) b", "- gradient \\(\\nabla_\\theta J\\) step",
  "- item:\n\n  $$\n  E = mc^2\n  $$", "Math:\n\n    \\[\n    E = mc^2\n    \\]",
  "```latex\n\\[\nE = mc^2\n\\]\n```", "run `echo $PATH$HOME` and `$x_i$` now",
  ...latex.map(text => `$$${text}$$`),
  "🇦🇺 🇺🇸 👨‍👩‍👧‍👦 é $x_i$",
];
const truncate = [];
for (const text of ["abcdef", "あいうえお", "a🇦🇺bc", "\x1b[31mあaいう\x1b[0m", "\x1b[31ma\x1b[0m\x1b[32mいう", "a\tb", "👩‍💻ab", "e\u0301abc", "\x1b]malformedあabc"]) {
  for (const width of [0, 1, 2, 3, 4, 7, 10]) {
    for (const ellipsis of ["...", "…", "", "あい", "\x1b[35m…\x1b[0m"]) {
      for (const pad of [false, true]) truncate.push([text, width, ellipsis, pad, truncateToWidth(text, width, ellipsis, pad)]);
    }
  }
}
const fixture = {
  unicode: process.versions.unicode,
  widths: [...widthInputs].map(text => [text, visibleWidth(text)]),
  latex: latex.map(text => [text, latexToUnicode(text)]),
  markdown: markdown.flatMap(text => [12, 40, 80].map(width => [text, width, new Markdown(text, 0, 0, theme).render(width).map(stripAnsi)])),
  truncate,
  wrap: ["🇦🇺🇺🇸🇳🇿", "👩‍💻 👨‍👩‍👧‍👦 é", "\x1b[4mあいうえお\x1b[0m", "\x1b]8;;https://x.test\x07link text\x1b]8;;\x07"].flatMap(text => [2, 3, 7].map(width => [text, width, wrapTextWithAnsi(text, width)])),
  strip: ["a\x1béb", "a\x1b👩b", "a\x1b\u2028b", "\x1b[31mé\x1b[0m", "\x1b]unfinished"].map(text => [text, stripAnsi(text).toWellFormed()]),
};
writeFileSync(new URL("./pr18_rendering.json", import.meta.url), JSON.stringify(fixture) + "\n");
console.log(Object.fromEntries(Object.entries(fixture).map(([key, value]) => [key, Array.isArray(value) ? value.length : value])));
