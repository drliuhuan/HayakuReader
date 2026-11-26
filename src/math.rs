use latex_rust::{
    Color, Dim, MathFont, MathStyle, PngBackground, PngOptions, layout_with_em_size_pt, parse,
    render_png,
};
use scraper::ElementRef;
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

#[derive(Clone)]
pub struct RenderedMath {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub logical_size: [f32; 2],
    pub baseline_from_top: f32,
    pub rgba: Vec<u8>,
}

#[derive(Hash, PartialEq, Eq)]
struct MathKey {
    latex: String,
    display: bool,
    font_size_tenths: u32,
    scale_hundredths: u32,
    color: [u8; 4],
}

pub struct MathCache {
    font: Option<&'static MathFont>,
    entries: HashMap<MathKey, Option<Arc<RenderedMath>>>,
}

impl Default for MathCache {
    fn default() -> Self {
        Self {
            font: shared_stix_font(),
            entries: HashMap::new(),
        }
    }
}

impl MathCache {
    pub fn render(
        &mut self,
        latex: &str,
        display: bool,
        font_size_px: f32,
        scale: f32,
        ink: [u8; 4],
    ) -> Option<Arc<RenderedMath>> {
        let trimmed = latex.trim();
        if trimmed.is_empty() {
            return None;
        }
        let scale = scale.clamp(1.0, 4.0);
        let font_size_px = font_size_px.clamp(8.0, 96.0);
        let key = MathKey {
            latex: trimmed.to_owned(),
            display,
            font_size_tenths: (font_size_px * 10.0).round() as u32,
            scale_hundredths: (scale * 100.0).round() as u32,
            color: ink,
        };
        if let Some(cached) = self.entries.get(&key) {
            return cached.clone();
        }
        let rendered = self
            .font
            .and_then(|font| render_latex(trimmed, font, display, font_size_px, scale, ink).ok())
            .map(Arc::new);
        if self.entries.len() > 1024 {
            self.entries.clear();
        }
        self.entries.insert(key, rendered.clone());
        rendered
    }
}

pub fn shared_stix_font() -> Option<&'static MathFont> {
    static FONT: OnceLock<Option<MathFont>> = OnceLock::new();
    FONT.get_or_init(|| MathFont::stix_two_math().ok()).as_ref()
}

pub fn sanitize_latex(raw: &str) -> String {
    let mut s = raw.trim().to_owned();
    if let Some(inner) = s.strip_prefix("$$").and_then(|t| t.strip_suffix("$$")) {
        s = inner.trim().to_owned();
    } else if let Some(inner) = s.strip_prefix(r"\[").and_then(|t| t.strip_suffix(r"\]")) {
        s = inner.trim().to_owned();
    } else if let Some(inner) = s.strip_prefix(r"\(").and_then(|t| t.strip_suffix(r"\)")) {
        s = inner.trim().to_owned();
    } else if s.len() >= 2 && s.starts_with('$') && s.ends_with('$') {
        s = s[1..s.len() - 1].trim().to_owned();
    }

    // Strip invisible MathML formatting characters that break TeX parsers/glyphs
    s = s
        .chars()
        .filter(|&c| {
            !matches!(
                c,
                '\u{2061}' | '\u{2062}' | '\u{2063}' | '\u{2064}' | '\u{200B}' | '\u{FEFF}'
            )
        })
        .collect();

    // Common LaTeX macros -> latex-rust equivalents
    let replacements = [
        (r"\boldsymbol{", r"\mathbf{"),
        (r"\bm{", r"\mathbf{"),
        (r"\operatorname{", r"\mathrm{"),
        (r"\begin{aligned}", r"\begin{matrix}"),
        (r"\end{aligned}", r"\end{matrix}"),
        (r"\begin{align*}", r"\begin{matrix}"),
        (r"\end{align*}", r"\end{matrix}"),
        (r"\begin{align}", r"\begin{matrix}"),
        (r"\end{align}", r"\end{matrix}"),
        (r"\begin{gathered}", r"\begin{matrix}"),
        (r"\end{gathered}", r"\end{matrix}"),
        (r"\begin{equation*}", ""),
        (r"\end{equation*}", ""),
        (r"\begin{equation}", ""),
        (r"\end{equation}", ""),
        (r"\displaystyle", ""),
        (r"\textstyle", ""),
        (r"\scriptstyle", ""),
        (r"\scriptscriptstyle", ""),
        (r"\nolimits", ""),
        (r"\limits", ""),
        (r"\nonumber", ""),
    ];
    for (pat, rep) in replacements {
        if s.contains(pat) {
            s = s.replace(pat, rep);
        }
    }
    s = strip_macro_arg(&s, r"\label{");
    s = strip_macro_arg(&s, r"\tag{");
    s.trim().to_owned()
}

fn strip_macro_arg(input: &str, macro_prefix: &str) -> String {
    let mut out = input.to_owned();
    while let Some(pos) = out.find(macro_prefix) {
        let brace_start = pos + macro_prefix.len() - 1;
        let mut depth = 0i32;
        let mut end = None;
        for (off, ch) in out[brace_start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(brace_start + off + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(e) = end {
            out.replace_range(pos..e, "");
        } else {
            break;
        }
    }
    out
}

pub fn render_latex(
    latex: &str,
    font: &MathFont,
    display: bool,
    font_size_px: f32,
    scale: f32,
    ink: [u8; 4],
) -> Result<RenderedMath, String> {
    let cleaned = sanitize_latex(latex);
    let ast = parse(&cleaned)
        .or_else(|_| {
            let unicode_translated = translate_symbol(&cleaned);
            parse(&unicode_translated)
        })
        .map_err(|e| e.to_string())?;
    let em_pt = Dim::ratio((font_size_px * 100.0).round() as i64, 100);
    let dpi = Dim::ratio((72.0 * scale * 100.0).round() as i64, 100);
    let style = if display {
        MathStyle::Display
    } else {
        MathStyle::Text
    };
    let boxed = layout_with_em_size_pt(&ast, font, style, &em_pt).map_err(|e| e.to_string())?;
    let height_em = f32::from_bits(boxed.height.to_ieee32_bits());
    let options = PngOptions {
        font_size_pt: em_pt,
        dpi,
        color: Color::rgb(ink[0], ink[1], ink[2]),
        background: PngBackground::Transparent,
        display,
    };
    let png_bytes = render_png(&boxed, font, &options).map_err(|e| e.to_string())?;
    let img = image::load_from_memory_with_format(&png_bytes, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?
        .into_rgba8();
    let (pixel_width, pixel_height) = img.dimensions();
    let logical_w = (pixel_width as f32 / scale).max(1.0);
    let logical_h = (pixel_height as f32 / scale).max(1.0);
    let baseline_from_top = (height_em * font_size_px).clamp(0.0, logical_h);
    Ok(RenderedMath {
        pixel_width,
        pixel_height,
        logical_size: [logical_w, logical_h],
        baseline_from_top,
        rgba: img.into_raw(),
    })
}

pub fn translate_symbol(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            // Invisible MathML operators & zero-width marks
            '\u{2061}' | '\u{2062}' | '\u{2063}' | '\u{2064}' | '\u{200B}' | '\u{FEFF}' => {}
            '\u{00A0}' | '\u{2009}' | '\u{2005}' | '\u{2004}' | '\u{200A}' => out.push_str(r"\,"),
            '′' => out.push('\''),
            '″' => out.push_str("''"),
            '≤' | '⩽' => out.push_str(r"\le "),
            '≥' | '⩾' => out.push_str(r"\ge "),
            '≠' => out.push_str(r"\ne "),
            '≈' => out.push_str(r"\approx "),
            '≡' => out.push_str(r"\equiv "),
            '∼' | '˜' => out.push_str(r"\sim "),
            '≃' => out.push_str(r"\simeq "),
            '≅' => out.push_str(r"\cong "),
            '∝' => out.push_str(r"\propto "),
            '×' => out.push_str(r"\times "),
            '÷' => out.push_str(r"\div "),
            '±' => out.push_str(r"\pm "),
            '∓' => out.push_str(r"\mp "),
            '·' | '⋅' | '∙' => out.push_str(r"\cdot "),
            '∘' => out.push_str(r"\circ "),
            '⊕' => out.push_str(r"\oplus "),
            '⊗' => out.push_str(r"\otimes "),
            '†' => out.push_str(r"\dagger "),
            '∞' => out.push_str(r"\infty "),
            '∑' => out.push_str(r"\sum "),
            '∏' => out.push_str(r"\prod "),
            '∫' => out.push_str(r"\int "),
            '∮' => out.push_str(r"\oint "),
            '∂' => out.push_str(r"\partial "),
            '∇' => out.push_str(r"\nabla "),
            '√' => out.push_str(r"\sqrt "),
            '→' | '⟶' => out.push_str(r"\to "),
            '←' | '⟵' => out.push_str(r"\leftarrow "),
            '↔' | '⟷' => out.push_str(r"\leftrightarrow "),
            '↦' => out.push_str(r"\mapsto "),
            '⇒' | '⟹' => out.push_str(r"\Rightarrow "),
            '⇐' => out.push_str(r"\Leftarrow "),
            '⇔' | '⟺' => out.push_str(r"\Leftrightarrow "),
            '↑' => out.push_str(r"\uparrow "),
            '↓' => out.push_str(r"\downarrow "),
            '∈' => out.push_str(r"\in "),
            '∉' => out.push_str(r"\notin "),
            '⊂' => out.push_str(r"\subset "),
            '⊃' => out.push_str(r"\supset "),
            '⊆' => out.push_str(r"\subseteq "),
            '⊇' => out.push_str(r"\supseteq "),
            '∪' => out.push_str(r"\cup "),
            '∩' => out.push_str(r"\cap "),
            '∖' => out.push_str(r"\setminus "),
            '∧' => out.push_str(r"\land "),
            '∨' => out.push_str(r"\lor "),
            '¬' => out.push_str(r"\neg "),
            '⊥' => out.push_str(r"\perp "),
            '∥' => out.push_str(r"\parallel "),
            '⟨' | '〈' => out.push_str(r"\langle "),
            '⟩' | '〉' => out.push_str(r"\rangle "),
            '‖' => out.push_str(r"\| "),
            '∀' => out.push_str(r"\forall "),
            '∃' => out.push_str(r"\exists "),
            '∅' => out.push_str(r"\emptyset "),
            '…' | '⋯' => out.push_str(r"\cdots "),
            '⋮' => out.push_str(r"\vdots "),
            '⋱' => out.push_str(r"\ddots "),
            'ℕ' => out.push_str(r"\mathbb{N}"),
            'ℤ' => out.push_str(r"\mathbb{Z}"),
            'ℚ' => out.push_str(r"\mathbb{Q}"),
            'ℝ' => out.push_str(r"\mathbb{R}"),
            'ℂ' => out.push_str(r"\mathbb{C}"),
            'ℓ' => out.push_str(r"\ell "),
            'ℏ' => out.push_str(r"\hbar "),
            'ℵ' => out.push_str(r"\aleph "),
            '−' | '–' => out.push('-'),
            'α' => out.push_str(r"\alpha "),
            'β' => out.push_str(r"\beta "),
            'γ' => out.push_str(r"\gamma "),
            'δ' => out.push_str(r"\delta "),
            'ε' | 'ϵ' => out.push_str(r"\epsilon "),
            'ζ' => out.push_str(r"\zeta "),
            'η' => out.push_str(r"\eta "),
            'θ' => out.push_str(r"\theta "),
            'ι' => out.push_str(r"\iota "),
            'κ' => out.push_str(r"\kappa "),
            'λ' => out.push_str(r"\lambda "),
            'μ' => out.push_str(r"\mu "),
            'ν' => out.push_str(r"\nu "),
            'ξ' => out.push_str(r"\xi "),
            'π' => out.push_str(r"\pi "),
            'ρ' => out.push_str(r"\rho "),
            'σ' => out.push_str(r"\sigma "),
            'τ' => out.push_str(r"\tau "),
            'υ' => out.push_str(r"\upsilon "),
            'φ' | 'ϕ' => out.push_str(r"\phi "),
            'χ' => out.push_str(r"\chi "),
            'ψ' => out.push_str(r"\psi "),
            'ω' => out.push_str(r"\omega "),
            'Γ' => out.push_str(r"\Gamma "),
            'Δ' => out.push_str(r"\Delta "),
            'Θ' => out.push_str(r"\Theta "),
            'Λ' => out.push_str(r"\Lambda "),
            'Ξ' => out.push_str(r"\Xi "),
            'Π' => out.push_str(r"\Pi "),
            'Σ' => out.push_str(r"\Sigma "),
            'Φ' => out.push_str(r"\Phi "),
            'Ψ' => out.push_str(r"\Psi "),
            'Ω' => out.push_str(r"\Omega "),
            _ => out.push(ch),
        }
    }
    out
}

fn is_big_operator(latex: &str) -> bool {
    matches!(
        latex.trim(),
        r"\sum"
            | r"\prod"
            | r"\int"
            | r"\oint"
            | r"\bigcup"
            | r"\bigcap"
            | r"\bigoplus"
            | r"\bigotimes"
            | r"\lim"
            | r"\limsup"
            | r"\liminf"
            | r"\sup"
            | r"\inf"
            | r"\max"
            | r"\min"
    )
}

fn wrap_script_base(base: &str) -> String {
    let trimmed = base.trim();
    if is_big_operator(trimmed)
        || (trimmed.len() == 1 && trimmed.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        trimmed.to_owned()
    } else {
        format!("{{{trimmed}}}")
    }
}

fn is_standard_math_func(name: &str) -> bool {
    matches!(
        name,
        "sin"
            | "cos"
            | "tan"
            | "cot"
            | "sec"
            | "csc"
            | "arcsin"
            | "arccos"
            | "arctan"
            | "sinh"
            | "cosh"
            | "tanh"
            | "coth"
            | "log"
            | "ln"
            | "lg"
            | "exp"
            | "lim"
            | "limsup"
            | "liminf"
            | "sup"
            | "inf"
            | "max"
            | "min"
            | "det"
            | "dim"
            | "gcd"
            | "lcm"
            | "ker"
            | "arg"
            | "deg"
            | "Pr"
    )
}

fn escape_fence(delim: &str) -> String {
    let t = delim.trim();
    if t.is_empty() {
        return ".".to_owned();
    }
    match t {
        "{" => r"\{".to_owned(),
        "}" => r"\}".to_owned(),
        "⟨" => r"\langle".to_owned(),
        "⟩" => r"\rangle".to_owned(),
        "‖" => r"\|".to_owned(),
        other => other.to_owned(),
    }
}

fn element_text(el: ElementRef<'_>) -> String {
    el.text().collect::<Vec<_>>().join("")
}

pub fn mathml_element_to_latex(el: ElementRef<'_>) -> String {
    let tag = el.value().name();
    let children: Vec<ElementRef<'_>> = el.child_elements().collect();
    match tag {
        "annotation" | "annotation-xml" => String::new(),
        "semantics" => {
            for child in &children {
                if child.value().name() == "annotation" {
                    let enc = child
                        .value()
                        .attr("encoding")
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if enc.contains("tex") || enc.contains("latex") {
                        let candidate = sanitize_latex(&element_text(*child));
                        if !candidate.is_empty() && parse(&candidate).is_ok() {
                            return candidate;
                        }
                    }
                }
            }
            children
                .into_iter()
                .find(|c| !matches!(c.value().name(), "annotation" | "annotation-xml"))
                .map(mathml_element_to_latex)
                .unwrap_or_default()
        }
        "math" | "mrow" | "mstyle" | "mpadded" | "mphantom" => {
            if children.is_empty() {
                return translate_symbol(element_text(el).trim());
            }
            children
                .into_iter()
                .map(mathml_element_to_latex)
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        }
        "mi" => {
            let raw = element_text(el);
            let translated = translate_symbol(raw.trim());
            let trimmed = translated.trim();
            if trimmed.is_empty() {
                return String::new();
            }
            if is_standard_math_func(trimmed) {
                return format!(r"\{trimmed} ");
            }
            let variant = el.value().attr("mathvariant").unwrap_or("");
            if trimmed.chars().all(|c| c.is_ascii_alphanumeric()) && trimmed.len() > 1 {
                return match variant {
                    "bold" | "bold-italic" => format!(r"\mathbf{{{trimmed}}}"),
                    _ => format!(r"\mathrm{{{trimmed}}}"),
                };
            }
            match variant {
                "bold" => format!(r"\mathbf{{{trimmed}}}"),
                "double-struck" => format!(r"\mathbb{{{trimmed}}}"),
                "script" => format!(r"\mathcal{{{trimmed}}}"),
                _ => trimmed.to_owned(),
            }
        }
        "mn" | "mo" => translate_symbol(element_text(el).trim()).trim().to_owned(),
        "mtext" => {
            let text = element_text(el);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                String::new()
            } else {
                let safe = trimmed.replace(['{', '}'], "");
                format!(r"\text{{{safe}}}")
            }
        }
        "mspace" => r"\,".to_owned(),
        "mfrac" => {
            let num = children
                .first()
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            let den = children
                .get(1)
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            format!(r"\frac{{{num}}}{{{den}}}")
        }
        "msqrt" => {
            let inner = children
                .into_iter()
                .map(mathml_element_to_latex)
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            format!(r"\sqrt{{{inner}}}")
        }
        "mroot" => {
            let base = children
                .first()
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            let idx = children
                .get(1)
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            format!(r"\sqrt[{idx}]{{{base}}}")
        }
        "msup" => {
            let base = children
                .first()
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            let sup = children
                .get(1)
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            format!("{}^{{{sup}}}", wrap_script_base(&base))
        }
        "msub" => {
            let base = children
                .first()
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            let sub = children
                .get(1)
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            format!("{}_{{{sub}}}", wrap_script_base(&base))
        }
        "msubsup" | "munderover" => {
            let base = children
                .first()
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            let sub = children
                .get(1)
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            let sup = children
                .get(2)
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            format!("{}_{{{sub}}}^{{{sup}}}", wrap_script_base(&base))
        }
        "mover" => {
            let base = children
                .first()
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            let over = children
                .get(1)
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            match over.trim() {
                "¯" | "‾" | "-" => format!(r"\overline{{{base}}}"),
                "^" | "ˆ" => format!(r"\hat{{{base}}}"),
                "~" | "˜" | r"\sim" => format!(r"\tilde{{{base}}}"),
                "→" | r"\to" => format!(r"\vec{{{base}}}"),
                "˙" | "." => format!(r"\dot{{{base}}}"),
                _ => format!("{}^{{{over}}}", wrap_script_base(&base)),
            }
        }
        "munder" => {
            let base = children
                .first()
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            let under = children
                .get(1)
                .copied()
                .map(mathml_element_to_latex)
                .unwrap_or_default();
            match under.trim() {
                "_" | "‾" | "¯" => format!(r"\underline{{{base}}}"),
                _ => format!("{}_{{{under}}}", wrap_script_base(&base)),
            }
        }
        "mfenced" => {
            let open = escape_fence(el.value().attr("open").unwrap_or("("));
            let close = escape_fence(el.value().attr("close").unwrap_or(")"));
            let sep = el.value().attr("separators").unwrap_or(",");
            let joiner = if sep.is_empty() {
                " ".to_owned()
            } else {
                let first_sep = sep.chars().next().unwrap_or(',');
                format!("{first_sep} ")
            };
            let body = children
                .into_iter()
                .map(mathml_element_to_latex)
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join(&joiner);
            format!(r"\left{open} {body} \right{close}")
        }
        "mtable" => {
            let rows = children
                .into_iter()
                .map(mathml_element_to_latex)
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join(r" \\ ");
            format!(r"\begin{{matrix}} {rows} \end{{matrix}}")
        }
        "mtr" | "mlabeledtr" => children
            .into_iter()
            .map(mathml_element_to_latex)
            .collect::<Vec<_>>()
            .join(" & "),
        "mtd" => children
            .into_iter()
            .map(mathml_element_to_latex)
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        _ => {
            if children.is_empty() {
                translate_symbol(element_text(el).trim())
            } else {
                children
                    .into_iter()
                    .map(mathml_element_to_latex)
                    .filter(|s| !s.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scraper::{Html, Selector};

    fn parse_math_fragment(html_str: &str) -> String {
        let doc = Html::parse_fragment(html_str);
        let sel = Selector::parse("math").unwrap();
        let el = doc.select(&sel).next().unwrap();
        mathml_element_to_latex(el)
    }

    #[test]
    fn renders_inline_and_display_latex_with_baseline() {
        let font = shared_stix_font().expect("STIX Two Math embedded");
        let inline =
            render_latex(r"\frac{1}{2} + \alpha^2", font, false, 19.0, 2.0, [20, 20, 20, 255])
                .expect("inline formula");
        let display =
            render_latex(r"\int_0^\infty e^{-x^2} dx", font, true, 19.0, 2.0, [240, 240, 240, 255])
                .expect("display formula");
        assert!(inline.pixel_width > 0 && inline.pixel_height > 0);
        assert!(inline.baseline_from_top > 0.0);
        assert!(display.pixel_width > 0 && display.pixel_height > 0);
        assert!(display.logical_size[1] > inline.logical_size[1]);
    }

    #[test]
    fn converts_academic_mathml_semantics_and_invisible_ops() {
        let mml = r#"<math xmlns="http://www.w3.org/1998/Math/MathML">
            <semantics>
                <mrow><munderover><mo>∑</mo><mrow><mi>i</mi><mo>=</mo><mn>1</mn></mrow><mi>n</mi></munderover><msub><mi>x</mi><mi>i</mi></msub></mrow>
                <annotation encoding="application/x-tex">\sum_{i=1}^{n} x_i</annotation>
            </semantics>
        </math>"#;
        let tex = parse_math_fragment(mml);
        assert_eq!(tex, r"\sum_{i=1}^{n} x_i");
        let font = shared_stix_font().unwrap();
        assert!(render_latex(&tex, font, true, 18.0, 1.0, [0, 0, 0, 255]).is_ok());

        // Fallback structural MathML without TeX annotation + U+2062 invisible times
        let pure_mml = "<math><mfrac><msqrt><mi>a</mi><mo>\u{2062}</mo><mi>b</mi></msqrt><msup><mi>c</mi><mn>2</mn></msup></mfrac></math>";
        let pure_tex = parse_math_fragment(pure_mml);
        assert_eq!(pure_tex, r"\frac{\sqrt{a b}}{c^{2}}");
        assert!(render_latex(&pure_tex, font, false, 18.0, 1.0, [0, 0, 0, 255]).is_ok());

        // Big sum without TeX annotation
        let sum_mml = "<math><munderover><mo>∑</mo><mrow><mi>k</mi><mo>=</mo><mn>0</mn></mrow><mi>∞</mi></munderover><mfrac><msup><mi>x</mi><mi>k</mi></msup><mrow><mi>k</mi><mo>!</mo></mrow></mfrac></math>";
        let sum_tex = parse_math_fragment(sum_mml);
        assert!(render_latex(&sum_tex, font, true, 18.0, 1.0, [0, 0, 0, 255]).is_ok());
    }
}
