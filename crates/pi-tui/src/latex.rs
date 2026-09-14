//! Port of packages/tui/src/latex.ts.

use unicode_general_category::{get_general_category, GeneralCategory};

const SYMBOLS: &[(&str, &str)] = &[
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ε"),
    ("varepsilon", "ε"),
    ("zeta", "ζ"),
    ("eta", "η"),
    ("theta", "θ"),
    ("vartheta", "ϑ"),
    ("iota", "ι"),
    ("kappa", "κ"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("nu", "ν"),
    ("xi", "ξ"),
    ("pi", "π"),
    ("varpi", "ϖ"),
    ("rho", "ρ"),
    ("varrho", "ϱ"),
    ("sigma", "σ"),
    ("varsigma", "ς"),
    ("tau", "τ"),
    ("upsilon", "υ"),
    ("phi", "ϕ"),
    ("varphi", "φ"),
    ("chi", "χ"),
    ("psi", "ψ"),
    ("omega", "ω"),
    ("Gamma", "Γ"),
    ("Delta", "Δ"),
    ("Theta", "Θ"),
    ("Lambda", "Λ"),
    ("Xi", "Ξ"),
    ("Pi", "Π"),
    ("Sigma", "Σ"),
    ("Upsilon", "Υ"),
    ("Phi", "Φ"),
    ("Psi", "Ψ"),
    ("Omega", "Ω"),
    ("sum", "∑"),
    ("prod", "∏"),
    ("coprod", "∐"),
    ("int", "∫"),
    ("iint", "∬"),
    ("iiint", "∭"),
    ("oint", "∮"),
    ("bigcup", "⋃"),
    ("bigcap", "⋂"),
    ("bigoplus", "⨁"),
    ("bigotimes", "⨂"),
    ("bigodot", "⨀"),
    ("bigvee", "⋁"),
    ("bigwedge", "⋀"),
    ("bigsqcup", "⨆"),
    ("pm", "±"),
    ("mp", "∓"),
    ("times", "×"),
    ("div", "÷"),
    ("cdot", "·"),
    ("ast", "∗"),
    ("star", "⋆"),
    ("circ", "∘"),
    ("bullet", "•"),
    ("oplus", "⊕"),
    ("ominus", "⊖"),
    ("otimes", "⊗"),
    ("oslash", "⊘"),
    ("odot", "⊙"),
    ("wedge", "∧"),
    ("land", "∧"),
    ("vee", "∨"),
    ("lor", "∨"),
    ("cap", "∩"),
    ("cup", "∪"),
    ("setminus", "∖"),
    ("sqcap", "⊓"),
    ("sqcup", "⊔"),
    ("uplus", "⊎"),
    ("dagger", "†"),
    ("ddagger", "‡"),
    ("leq", "≤"),
    ("le", "≤"),
    ("geq", "≥"),
    ("ge", "≥"),
    ("neq", "≠"),
    ("ne", "≠"),
    ("equiv", "≡"),
    ("sim", "∼"),
    ("simeq", "≃"),
    ("approx", "≈"),
    ("cong", "≅"),
    ("propto", "∝"),
    ("ll", "≪"),
    ("gg", "≫"),
    ("subset", "⊂"),
    ("supset", "⊃"),
    ("subseteq", "⊆"),
    ("supseteq", "⊇"),
    ("sqsubseteq", "⊑"),
    ("sqsupseteq", "⊒"),
    ("in", "∈"),
    ("ni", "∋"),
    ("notin", "∉"),
    ("models", "⊨"),
    ("vdash", "⊢"),
    ("dashv", "⊣"),
    ("perp", "⊥"),
    ("parallel", "∥"),
    ("mid", "∣"),
    ("asymp", "≍"),
    ("doteq", "≐"),
    ("prec", "≺"),
    ("succ", "≻"),
    ("preceq", "⪯"),
    ("succeq", "⪰"),
    ("triangleq", "≜"),
    ("coloneqq", "≔"),
    ("coloneq", "≔"),
    ("to", "→"),
    ("rightarrow", "→"),
    ("leftarrow", "←"),
    ("gets", "←"),
    ("leftrightarrow", "↔"),
    ("Rightarrow", "⇒"),
    ("Leftarrow", "⇐"),
    ("Leftrightarrow", "⇔"),
    ("iff", "⇔"),
    ("implies", "⇒"),
    ("impliedby", "⇐"),
    ("mapsto", "↦"),
    ("longrightarrow", "⟶"),
    ("longleftarrow", "⟵"),
    ("Longrightarrow", "⟹"),
    ("Longleftarrow", "⟸"),
    ("longmapsto", "⟼"),
    ("uparrow", "↑"),
    ("downarrow", "↓"),
    ("updownarrow", "↕"),
    ("Uparrow", "⇑"),
    ("Downarrow", "⇓"),
    ("hookrightarrow", "↪"),
    ("hookleftarrow", "↩"),
    ("rightharpoonup", "⇀"),
    ("leftharpoonup", "↼"),
    ("rightrightarrows", "⇉"),
    ("rightleftarrows", "⇄"),
    ("leadsto", "⇝"),
    ("nearrow", "↗"),
    ("searrow", "↘"),
    ("nwarrow", "↖"),
    ("swarrow", "↙"),
    ("infty", "∞"),
    ("partial", "∂"),
    ("nabla", "∇"),
    ("forall", "∀"),
    ("exists", "∃"),
    ("nexists", "∄"),
    ("emptyset", "∅"),
    ("varnothing", "∅"),
    ("neg", "¬"),
    ("lnot", "¬"),
    ("angle", "∠"),
    ("triangle", "△"),
    ("square", "□"),
    ("Box", "□"),
    ("blacksquare", "■"),
    ("diamond", "⋄"),
    ("Diamond", "◇"),
    ("aleph", "ℵ"),
    ("hbar", "ℏ"),
    ("ell", "ℓ"),
    ("Re", "ℜ"),
    ("Im", "ℑ"),
    ("wp", "℘"),
    ("top", "⊤"),
    ("bot", "⊥"),
    ("flat", "♭"),
    ("sharp", "♯"),
    ("natural", "♮"),
    ("checkmark", "✓"),
    ("degree", "°"),
    ("prime", "′"),
    ("therefore", "∴"),
    ("because", "∵"),
    ("dots", "…"),
    ("ldots", "…"),
    ("dotsc", "…"),
    ("dotso", "…"),
    ("cdots", "⋯"),
    ("dotsb", "⋯"),
    ("vdots", "⋮"),
    ("ddots", "⋱"),
    ("langle", "⟨"),
    ("rangle", "⟩"),
    ("lceil", "⌈"),
    ("rceil", "⌉"),
    ("lfloor", "⌊"),
    ("rfloor", "⌋"),
    ("lvert", "|"),
    ("rvert", "|"),
    ("vert", "|"),
    ("lVert", "‖"),
    ("rVert", "‖"),
    ("Vert", "‖"),
];

const OPERATOR_NAMES: &[&str] = &[
    "log", "ln", "lg", "exp", "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos",
    "arctan", "sinh", "cosh", "tanh", "coth", "min", "max", "argmin", "argmax", "arg", "sup",
    "inf", "lim", "limsup", "liminf", "det", "dim", "ker", "deg", "gcd", "hom", "Pr", "tr", "Tr",
    "rank", "diag", "sgn", "softmax", "mod", "bmod",
];

/// Single-character escapes (`\{` -> `{`) and spacing commands.
const ESCAPES: &[(&str, &str)] = &[
    ("{", "{"),
    ("}", "}"),
    ("%", "%"),
    ("$", "$"),
    ("&", "&"),
    ("#", "#"),
    ("_", "_"),
    ("|", "‖"),
    ("\\", "\n"),
    (" ", " "),
    (",", " "),
    (";", " "),
    (":", " "),
    ("!", ""),
];

const SPACING_COMMANDS: &[(&str, &str)] = &[
    ("quad", "  "),
    ("qquad", "    "),
    ("thinspace", " "),
    ("enspace", " "),
    ("medspace", " "),
    ("thickspace", " "),
];

/// Accent commands -> combining character appended to each character.
const ACCENTS: &[(&str, &str)] = &[
    ("hat", "̂"),
    ("widehat", "̂"),
    ("bar", "̄"),
    ("overline", "̅"),
    ("underline", "̲"),
    ("vec", "⃗"),
    ("tilde", "̃"),
    ("widetilde", "̃"),
    ("dot", "̇"),
    ("ddot", "̈"),
    ("breve", "̆"),
    ("check", "̌"),
    ("acute", "́"),
    ("grave", "̀"),
    ("mathring", "̊"),
];

const SUPERSCRIPTS: &[(&str, &str)] = &[
    ("0", "⁰"),
    ("1", "¹"),
    ("2", "²"),
    ("3", "³"),
    ("4", "⁴"),
    ("5", "⁵"),
    ("6", "⁶"),
    ("7", "⁷"),
    ("8", "⁸"),
    ("9", "⁹"),
    ("+", "⁺"),
    ("-", "⁻"),
    ("−", "⁻"),
    ("=", "⁼"),
    ("(", "⁽"),
    (")", "⁾"),
    ("*", "*"),
    ("a", "ᵃ"),
    ("b", "ᵇ"),
    ("c", "ᶜ"),
    ("d", "ᵈ"),
    ("e", "ᵉ"),
    ("f", "ᶠ"),
    ("g", "ᵍ"),
    ("h", "ʰ"),
    ("i", "ⁱ"),
    ("j", "ʲ"),
    ("k", "ᵏ"),
    ("l", "ˡ"),
    ("m", "ᵐ"),
    ("n", "ⁿ"),
    ("o", "ᵒ"),
    ("p", "ᵖ"),
    ("r", "ʳ"),
    ("s", "ˢ"),
    ("t", "ᵗ"),
    ("u", "ᵘ"),
    ("v", "ᵛ"),
    ("w", "ʷ"),
    ("x", "ˣ"),
    ("y", "ʸ"),
    ("z", "ᶻ"),
    ("A", "ᴬ"),
    ("B", "ᴮ"),
    ("D", "ᴰ"),
    ("E", "ᴱ"),
    ("G", "ᴳ"),
    ("H", "ᴴ"),
    ("I", "ᴵ"),
    ("J", "ᴶ"),
    ("K", "ᴷ"),
    ("L", "ᴸ"),
    ("M", "ᴹ"),
    ("N", "ᴺ"),
    ("O", "ᴼ"),
    ("P", "ᴾ"),
    ("R", "ᴿ"),
    ("T", "ᵀ"),
    ("U", "ᵁ"),
    ("V", "ⱽ"),
    ("W", "ᵂ"),
    ("β", "ᵝ"),
    ("γ", "ᵞ"),
    ("δ", "ᵟ"),
    ("θ", "ᶿ"),
    ("ϕ", "ᵠ"),
    ("φ", "ᵠ"),
    ("χ", "ᵡ"),
];

const SUBSCRIPTS: &[(&str, &str)] = &[
    ("0", "₀"),
    ("1", "₁"),
    ("2", "₂"),
    ("3", "₃"),
    ("4", "₄"),
    ("5", "₅"),
    ("6", "₆"),
    ("7", "₇"),
    ("8", "₈"),
    ("9", "₉"),
    ("+", "₊"),
    ("-", "₋"),
    ("−", "₋"),
    ("=", "₌"),
    ("(", "₍"),
    (")", "₎"),
    ("a", "ₐ"),
    ("e", "ₑ"),
    ("h", "ₕ"),
    ("i", "ᵢ"),
    ("j", "ⱼ"),
    ("k", "ₖ"),
    ("l", "ₗ"),
    ("m", "ₘ"),
    ("n", "ₙ"),
    ("o", "ₒ"),
    ("p", "ₚ"),
    ("r", "ᵣ"),
    ("s", "ₛ"),
    ("t", "ₜ"),
    ("u", "ᵤ"),
    ("v", "ᵥ"),
    ("x", "ₓ"),
    ("β", "ᵦ"),
    ("γ", "ᵧ"),
    ("ρ", "ᵨ"),
    ("ϕ", "ᵩ"),
    ("φ", "ᵩ"),
    ("χ", "ᵪ"),
];

const COMMON_FRACTIONS: &[(&str, &str)] = &[
    ("1/2", "½"),
    ("1/3", "⅓"),
    ("2/3", "⅔"),
    ("1/4", "¼"),
    ("3/4", "¾"),
    ("1/5", "⅕"),
    ("2/5", "⅖"),
    ("3/5", "⅗"),
    ("4/5", "⅘"),
    ("1/6", "⅙"),
    ("5/6", "⅚"),
    ("1/8", "⅛"),
];

fn lookup<'a>(table: &'a [(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn lookup_char<'a>(table: &'a [(&'a str, &'a str)], key: char) -> Option<&'a str> {
    let mut buf = [0u8; 4];
    lookup(table, key.encode_utf8(&mut buf))
}

fn in_set(table: &[&str], key: &str) -> bool {
    table.contains(&key)
}

/// Code point of the styled "A" in the Mathematical Alphanumeric block.
#[derive(Debug, Clone)]
struct AlphabetStyle {
    upper: Option<u32>,
    lower: Option<u32>,
    digit: Option<u32>,
    /// Letters whose styled forms predate the block and live elsewhere in the BMP.
    exceptions: &'static [(&'static str, &'static str)],
}

const ALPHABET_MATHBB: AlphabetStyle = AlphabetStyle {
    upper: Some(0x1d538),
    lower: Some(0x1d552),
    digit: Some(0x1d7d8),
    exceptions: &[
        ("C", "\u{2102}"),
        ("H", "\u{210d}"),
        ("N", "\u{2115}"),
        ("P", "\u{2119}"),
        ("Q", "\u{211a}"),
        ("R", "\u{211d}"),
        ("Z", "\u{2124}"),
    ],
};
const ALPHABET_MATHBF: AlphabetStyle = AlphabetStyle {
    upper: Some(0x1d400),
    lower: Some(0x1d41a),
    digit: Some(0x1d7ce),
    exceptions: &[],
};
const ALPHABET_MATHCAL: AlphabetStyle = AlphabetStyle {
    upper: Some(0x1d49c),
    lower: Some(0x1d4b6),
    digit: None,
    exceptions: &[
        ("B", "\u{212c}"),
        ("E", "\u{2130}"),
        ("F", "\u{2131}"),
        ("H", "\u{210b}"),
        ("I", "\u{2110}"),
        ("L", "\u{2112}"),
        ("M", "\u{2133}"),
        ("R", "\u{211b}"),
        ("e", "\u{212f}"),
        ("g", "\u{210a}"),
        ("o", "\u{2134}"),
    ],
};
const ALPHABET_MATHFRAK: AlphabetStyle = AlphabetStyle {
    upper: Some(0x1d504),
    lower: Some(0x1d51e),
    digit: None,
    exceptions: &[
        ("C", "\u{212d}"),
        ("H", "\u{210c}"),
        ("I", "\u{2111}"),
        ("R", "\u{211c}"),
        ("Z", "\u{2128}"),
    ],
};

fn alphabet(name: &str) -> Option<&'static AlphabetStyle> {
    Some(match name {
        "mathbb" => &ALPHABET_MATHBB,
        "mathbf" => &ALPHABET_MATHBF,
        "boldsymbol" => &ALPHABET_MATHBF,
        "bm" => &ALPHABET_MATHBF,
        "textbf" => &ALPHABET_MATHBF,
        "mathcal" => &ALPHABET_MATHCAL,
        "mathscr" => &ALPHABET_MATHCAL,
        "mathfrak" => &ALPHABET_MATHFRAK,
        _ => return None,
    })
}

/// Text-mode commands: their argument is literal text, so ^ and _ stay as-is.
const TEXT_COMMANDS: &[&str] = &[
    "text", "textrm", "textit", "textsf", "texttt", "mbox", "hbox",
];

/// Math-mode font commands rendered unstyled; scripts inside still apply.
const MATH_FONT_COMMANDS: &[&str] = &[
    "mathrm",
    "mathit",
    "mathsf",
    "mathtt",
    "mathnormal",
    "operatorname",
];

/// Size/style commands that take no argument and render as nothing.
const IGNORED_COMMANDS: &[&str] = &[
    "left",
    "right",
    "big",
    "Big",
    "bigg",
    "Bigg",
    "bigl",
    "bigr",
    "bigm",
    "Bigl",
    "Bigr",
    "Bigm",
    "biggl",
    "biggr",
    "Biggl",
    "Biggr",
    "displaystyle",
    "textstyle",
    "scriptstyle",
    "limits",
    "nolimits",
    "middle",
    "allowbreak",
    "nonumber",
    "notag",
];

fn style_alphabet(text: &str, style: &AlphabetStyle) -> String {
    let mut result = String::new();
    for ch in text.chars() {
        let key = ch.to_string();
        if let Some(exception) = lookup(style.exceptions, &key) {
            result.push_str(exception);
        } else if ch >= 'A' && ch <= 'Z' && style.upper.is_some() {
            let cp = style.upper.unwrap() + (ch as u32) - 0x41;
            result.push(char::from_u32(cp).unwrap_or(ch));
        } else if ch >= 'a' && ch <= 'z' && style.lower.is_some() {
            let cp = style.lower.unwrap() + (ch as u32) - 0x61;
            result.push(char::from_u32(cp).unwrap_or(ch));
        } else if ch >= '0' && ch <= '9' && style.digit.is_some() {
            let cp = style.digit.unwrap() + (ch as u32) - 0x30;
            result.push(char::from_u32(cp).unwrap_or(ch));
        } else {
            result.push(ch);
        }
    }
    result
}

/// Map every character through a sub/superscript table, or None if any character is missing.
fn map_script(text: &str, table: &[(&str, &str)]) -> Option<String> {
    let mut result = String::new();
    for ch in text.chars() {
        let mapped = lookup_char(table, ch)?;
        result.push_str(mapped);
    }
    Some(result)
}

/// True when a fraction/sqrt operand reads unambiguously without parentheses.
///
/// Port of `isSimpleOperand` (packages/tui/src/latex.ts:572-574):
/// `[...text].length === 1 || /^[\p{L}\p{N}\p{M}]+$/u.test(text)`. The general
/// categories - not `char::is_alphanumeric`, which is `L*`/`N*` only - are what make
/// a combining mark a simple operand, so `\frac{\vec{v}}{2}` renders `v⃗/2` and not
/// `(v⃗)/2` (markdown-latex.test.ts:86-88).
fn is_simple_operand(text: &str) -> bool {
    if text.chars().count() == 1 {
        return true;
    }
    !text.is_empty()
        && text.chars().all(|c| {
            matches!(
                get_general_category(c),
                GeneralCategory::UppercaseLetter
                    | GeneralCategory::LowercaseLetter
                    | GeneralCategory::TitlecaseLetter
                    | GeneralCategory::ModifierLetter
                    | GeneralCategory::OtherLetter
                    | GeneralCategory::DecimalNumber
                    | GeneralCategory::LetterNumber
                    | GeneralCategory::OtherNumber
                    | GeneralCategory::NonspacingMark
                    | GeneralCategory::SpacingMark
                    | GeneralCategory::EnclosingMark
            )
        })
}

fn parenthesize(text: &str) -> String {
    if is_simple_operand(text) {
        text.to_string()
    } else {
        format!("({text})")
    }
}

struct LatexParser {
    src: Vec<char>,
    pos: usize,
    text_mode: bool,
}

impl LatexParser {
    fn new(src: &str) -> Self {
        Self {
            src: src.chars().collect(),
            pos: 0,
            text_mode: false,
        }
    }

    fn parse(&mut self) -> String {
        self.parse_sequence()
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }

    /// Render atoms (with attached scripts) until a closing brace or end of input.
    fn parse_sequence(&mut self) -> String {
        let mut result = String::new();
        while self.pos < self.src.len() {
            let ch = self.src[self.pos];
            if ch == '}' {
                break;
            }
            if (ch == '^' || ch == '_') && !self.text_mode {
                self.pos += 1;
                let table = if ch == '^' { SUPERSCRIPTS } else { SUBSCRIPTS };
                result.push_str(&self.parse_script(table, ch));
                continue;
            }
            if ch == '&' {
                // Alignment marker: becomes a separating space unless one is there.
                self.pos += 1;
                if !result.ends_with(is_math_whitespace) {
                    result.push(' ');
                }
                continue;
            }
            let atom = self.parse_atom();
            if let Some(atom) = atom {
                result.push_str(&atom);
            }
        }
        result
    }

    /// Render the next atom: a group, a command, or a single character.
    fn parse_atom(&mut self) -> Option<String> {
        if self.pos >= self.src.len() {
            return None;
        }
        let ch = self.src[self.pos];
        if ch == '{' {
            return Some(self.parse_group());
        }
        if ch == '\\' {
            return Some(self.parse_command());
        }
        self.pos += 1;
        Some(ch.to_string())
    }

    /// Consume "{...}" and render its contents. Assumes "{" at the current position.
    fn parse_group(&mut self) -> String {
        self.pos += 1; // consume "{"
        let content = self.parse_sequence();
        if self.peek() == Some('}') {
            self.pos += 1;
        }
        content
    }

    /// Consume "[...]" and render its contents, or return None when absent.
    fn parse_optional_bracket(&mut self) -> Option<String> {
        if self.peek() != Some('[') {
            return None;
        }
        let end = self.src[self.pos + 1..].iter().position(|c| *c == ']')?;
        let end = self.pos + 1 + end;
        let slice: String = self.src[self.pos + 1..end].iter().collect();
        let content = LatexParser::new(&slice).parse_sequence();
        self.pos = end + 1;
        Some(content)
    }

    /// Render the next required argument: a braced group, or a single atom (TeX allows \frac12).
    fn parse_argument(&mut self) -> String {
        while self.pos < self.src.len() && is_math_whitespace(self.src[self.pos]) {
            self.pos += 1;
        }
        if self.peek() == Some('{') {
            return self.parse_group();
        }
        self.parse_atom().unwrap_or_default()
    }

    /// Render a ^ or _ script. The operator character itself is already consumed.
    fn parse_script(&mut self, table: &[(&str, &str)], operator: char) -> String {
        let content = self.parse_argument();
        if let Some(mapped) = map_script(&content, table) {
            return mapped;
        }
        // No Unicode form for every character: keep lossless TeX notation (x_θ).
        if content.chars().count() == 1 {
            format!("{operator}{content}")
        } else {
            format!("{operator}{{{content}}}")
        }
    }

    /// Render a command. Assumes "\" at the current position.
    fn parse_command(&mut self) -> String {
        self.pos += 1; // consume "\"
        if self.pos >= self.src.len() {
            return String::new();
        }

        let ch = self.src[self.pos];
        if !ch.is_ascii_alphabetic() {
            self.pos += 1;
            let key = ch.to_string();
            return lookup(ESCAPES, &key).unwrap_or(&key).to_string();
        }

        let mut name = String::new();
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_alphabetic() {
            name.push(self.src[self.pos]);
            self.pos += 1;
        }
        if self.peek() == Some('*') {
            self.pos += 1; // operatorname*, section* etc.
        }

        if let Some(symbol) = lookup(SYMBOLS, &name) {
            return symbol.to_string();
        }
        if in_set(OPERATOR_NAMES, &name) {
            return name;
        }
        if let Some(spacing) = lookup(SPACING_COMMANDS, &name) {
            return spacing.to_string();
        }
        if in_set(IGNORED_COMMANDS, &name) {
            if name == "left" || name == "right" {
                return self.parse_delimiter();
            }
            return String::new();
        }
        if in_set(TEXT_COMMANDS, &name) {
            let was_text_mode = self.text_mode;
            self.text_mode = true;
            let content = self.parse_argument();
            self.text_mode = was_text_mode;
            return content;
        }
        if in_set(MATH_FONT_COMMANDS, &name) {
            return self.parse_argument();
        }
        if let Some(style) = alphabet(&name) {
            let arg = self.parse_argument();
            return style_alphabet(&arg, style);
        }
        if let Some(accent) = lookup(ACCENTS, &name) {
            let content = self.parse_argument();
            let mut out = String::new();
            for c in content.chars() {
                if is_math_whitespace(c) {
                    out.push(c);
                } else {
                    out.push(c);
                    out.push_str(accent);
                }
            }
            return out;
        }
        match name.as_str() {
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let numerator = self.parse_argument();
                let denominator = self.parse_argument();
                if let Some(common) =
                    lookup(COMMON_FRACTIONS, &format!("{numerator}/{denominator}"))
                {
                    return common.to_string();
                }
                format!(
                    "{}/{}",
                    parenthesize(&numerator),
                    parenthesize(&denominator)
                )
            }
            "binom" => {
                let top = self.parse_argument();
                let bottom = self.parse_argument();
                format!("C({top},{bottom})")
            }
            "sqrt" => {
                let index = self.parse_optional_bracket();
                let radicand = self.parse_argument();
                let operand = if is_simple_operand(&radicand) {
                    radicand
                } else {
                    format!("({radicand})")
                };
                match index {
                    None => format!("\u{221a}{operand}"),
                    Some(index) if index == "3" => format!("\u{221b}{operand}"),
                    Some(index) if index == "4" => format!("\u{221c}{operand}"),
                    Some(index) => {
                        let mapped =
                            map_script(&index, SUPERSCRIPTS).unwrap_or_else(|| format!("^{index}"));
                        format!("{mapped}\u{221a}{operand}")
                    }
                }
            }
            "not" => {
                let negated = self.parse_atom().unwrap_or_default();
                format!("{negated}\u{338}")
            }
            "begin" | "end" => {
                self.parse_argument(); // environment name
                String::new()
            }
            "stackrel" | "overset" => {
                let above = self.parse_argument();
                let base = self.parse_argument();
                match map_script(&above, SUPERSCRIPTS) {
                    Some(mapped) => format!("{base}{mapped}"),
                    None => format!("{base}^{{{above}}}"),
                }
            }
            "underset" => {
                let below = self.parse_argument();
                let base = self.parse_argument();
                match map_script(&below, SUBSCRIPTS) {
                    Some(mapped) => format!("{base}{mapped}"),
                    None => format!("{base}_{{{below}}}"),
                }
            }
            _ => name,
        }
    }

    /// Render the delimiter following \left or \right ("." means invisible).
    fn parse_delimiter(&mut self) -> String {
        while self.pos < self.src.len() && is_math_whitespace(self.src[self.pos]) {
            self.pos += 1;
        }
        let ch = match self.peek() {
            Some(c) => c,
            None => return String::new(),
        };
        if ch == '.' {
            self.pos += 1;
            return String::new();
        }
        if ch == '\\' {
            return self.parse_command();
        }
        self.pos += 1;
        ch.to_string()
    }
}

// ECMAScript \s includes BOM and excludes NEL.
fn is_math_whitespace(c: char) -> bool {
    c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace())
}

/// Convert LaTeX math source to Unicode plain text.
pub fn latex_to_unicode(tex: &str) -> String {
    let parsed = LatexParser::new(tex).parse();
    let collapsed = collapse_spaces(&parsed);
    collapse_blank_lines(&collapsed)
}

/// Port of `.replace(/[^\S\n]{2,}/g, " ")` - collapse runs of 2+ non-newline spaces.
fn collapse_spaces(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c != '\n' && is_math_whitespace(c) {
            let mut j = i;
            while j < chars.len() && chars[j] != '\n' && is_math_whitespace(chars[j]) {
                j += 1;
            }
            if j - i >= 2 {
                out.push(' ');
            } else {
                out.push(c);
            }
            i = j;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Port of `.replace(/\n\s*\n/g, "\n")`.
///
/// `\s` includes `\n`, so the middle run is greedy and then backtracks to its last
/// newline: three consecutive newlines are a single match, and `a\n\n b` matches with
/// an empty middle run (`\s*` gives up the space) so the space survives the collapse.
fn collapse_blank_lines(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '\n' {
            let mut end = i + 1;
            while end < chars.len() && is_math_whitespace(chars[end]) {
                end += 1;
            }
            // Backtrack to the last newline the greedy `\s*` swallowed.
            if let Some(close) = chars[i + 1..end].iter().rposition(|c| *c == '\n') {
                out.push('\n');
                i = i + 1 + close + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_symbols_and_operators() {
        assert_eq!(latex_to_unicode(r"\alpha + \beta"), "\u{3b1} + \u{3b2}");
        assert_eq!(latex_to_unicode(r"\log x"), "log x");
    }

    #[test]
    fn scripts_use_unicode_when_available() {
        assert_eq!(latex_to_unicode("x_{t-k}"), "x\u{209c}\u{208b}\u{2096}");
        assert_eq!(latex_to_unicode("x^2"), "x\u{b2}");
        assert_eq!(latex_to_unicode(r"x_\theta"), "x_\u{3b8}");
    }

    /// `SUPERSCRIPTS` / `SUBSCRIPTS` carry 7 Greek forms each in the TS
    /// (packages/tui/src/latex.ts:375-381, 418-423).
    #[test]
    fn greek_letters_use_super_and_subscript_forms() {
        assert_eq!(latex_to_unicode(r"x^\beta"), "x\u{1d5d}");
        assert_eq!(latex_to_unicode(r"x^\gamma"), "x\u{1d5e}");
        assert_eq!(latex_to_unicode(r"x^\delta"), "x\u{1d5f}");
        assert_eq!(latex_to_unicode(r"x^\theta"), "x\u{1dbf}");
        assert_eq!(latex_to_unicode(r"x_\beta"), "x\u{1d66}");
        assert_eq!(latex_to_unicode(r"x_\gamma"), "x\u{1d67}");
        assert_eq!(latex_to_unicode(r"x_\rho"), "x\u{1d68}");
        assert_eq!(latex_to_unicode(r"x_\chi"), "x\u{1d6a}");
    }

    /// `isSimpleOperand` is `/^[\p{L}\p{N}\p{M}]+$/u` (latex.ts:572-574), so a
    /// combining mark counts as a simple operand and the parentheses are dropped
    /// (markdown-latex.test.ts:86-88).
    #[test]
    fn accented_operand_skips_parentheses() {
        assert_eq!(latex_to_unicode(r"\frac{\vec{v}}{2}"), "v\u{20d7}/2");
        assert_eq!(latex_to_unicode(r"\frac{\hat{x}}{2}"), "x\u{302}/2");
        // A non-mark, non-letter still needs parentheses.
        assert_eq!(latex_to_unicode(r"\frac{-1}{2}"), "(-1)/2");
    }

    /// `latexToUnicode` ends with `.replace(/\n\s*\n/g, "\n")` (latex.ts:833), and
    /// `\s` includes `\n`, so three newlines collapse to one.
    #[test]
    fn three_newlines_collapse_to_one() {
        assert_eq!(latex_to_unicode("a\n\n\nb"), "a\nb");
        // `\s*` is greedy then backtracks, so the space survives in `a\n\n b`.
        assert_eq!(latex_to_unicode("a\n\n b"), "a\n b");
        assert_eq!(latex_to_unicode("a\n \n\n b"), "a\n b");
        assert_eq!(latex_to_unicode("a\nb"), "a\nb");
    }

    #[test]
    fn fractions_and_sqrt_degrade_to_linear() {
        assert_eq!(latex_to_unicode(r"\frac{a}{b}"), "a/b");
        assert_eq!(latex_to_unicode(r"\frac{1}{2}"), "\u{bd}");
        assert_eq!(latex_to_unicode(r"\sqrt{x}"), "\u{221a}x");
        assert_eq!(latex_to_unicode(r"\sqrt[3]{x}"), "\u{221b}x");
    }

    #[test]
    fn unknown_commands_drop_the_backslash() {
        assert_eq!(latex_to_unicode(r"\notacommand"), "notacommand");
        assert_eq!(latex_to_unicode(r"\left( x \right)"), "( x )");
    }

    #[test]
    fn double_backslash_becomes_newline_and_collapses_blanks() {
        assert_eq!(latex_to_unicode(r"a \\ b"), "a \n b");
    }
}
