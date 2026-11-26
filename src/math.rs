use latex_rust::{
    Color, Dim, MathFont, MathStyle, PngBackground, PngOptions, layout_with_em_size_pt, parse,
    render_png,
};
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

pub fn render_latex(
    latex: &str,
    font: &MathFont,
    display: bool,
    font_size_px: f32,
    scale: f32,
    ink: [u8; 4],
) -> Result<RenderedMath, String> {
    let ast = parse(latex).map_err(|e| e.to_string())?;
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

pub fn shared_stix_font() -> Option<&'static MathFont> {
    static FONT: OnceLock<Option<MathFont>> = OnceLock::new();
    FONT.get_or_init(|| MathFont::stix_two_math().ok()).as_ref()
}

fn translate_symbol(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '≤' | '⩽' => out.push_str(r"\le "),
            '≥' | '⩾' => out.push_str(r"\ge "),
            '≠' => out.push_str(r"\ne "),
            '≈' => out.push_str(r"\approx "),
            '×' => out.push_str(r"\times "),
            '÷' => out.push_str(r"\div "),
            '±' => out.push_str(r"\pm "),
            '∓' => out.push_str(r"\mp "),
            '·' | '⋅' => out.push_str(r"\cdot "),
            '∞' => out.push_str(r"\infty "),
            '∑' => out.push_str(r"\sum "),
            '∏' => out.push_str(r"\prod "),
            '∫' => out.push_str(r"\int "),
            '∂' => out.push_str(r"\partial "),
            '∇' => out.push_str(r"\nabla "),
            '√' => out.push_str(r"\sqrt "),
            '→' | '⟶' => out.push_str(r"\to "),
            '←' | '⟵' => out.push_str(r"\leftarrow "),
            '⇒' => out.push_str(r"\Rightarrow "),
            '⇔' => out.push_str(r"\Leftrightarrow "),
            '∈' => out.push_str(r"\in "),
            '∉' => out.push_str(r"\notin "),
            '⊂' => out.push_str(r"\subset "),
            '⊆' => out.push_str(r"\subseteq "),
            '∪' => out.push_str(r"\cup "),
            '∩' => out.push_str(r"\cap "),
            '∀' => out.push_str(r"\forall "),
            '∃' => out.push_str(r"\exists "),
            '∅' => out.push_str(r"\emptyset "),
            '…' | '⋯' => out.push_str(r"\cdots "),
            '⋮' => out.push_str(r"\vdots "),
            '⋱' => out.push_str(r"\ddots "),
            '−' => out.push('-'),
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

pub fn mathml_to_latex(node: roxmltree::Node<'_, '_>) -> String {
    let children: Vec<roxmltree::Node<'_, '_>> =
        node.children().filter(|c| c.is_element()).collect();
    let tag = node.tag_name().name();
    match tag {
        "math" | "mrow" | "mstyle" | "mpadded" | "mphantom" => {
            if children.is_empty() {
                return translate_symbol(node.text().unwrap_or("").trim());
            }
            children
                .into_iter()
                .map(mathml_to_latex)
                .collect::<Vec<_>>()
                .join(" ")
        }
        "mi" | "mn" | "mo" => translate_symbol(node.text().unwrap_or("").trim()),
        "mtext" => {
            let text = node.text().unwrap_or("").trim();
            if text.is_empty() {
                String::new()
            } else {
                format!(r"\text{{{}}}", text.replace(['{', '}'], ""))
            }
        }
        "mspace" => r"\,".to_owned(),
        "mfrac" => {
            let num = children.first().copied().map(mathml_to_latex).unwrap_or_default();
            let den = children.get(1).copied().map(mathml_to_latex).unwrap_or_default();
            format!(r"\frac{{{num}}}{{{den}}}")
        }
        "msqrt" => {
            let inner = children
                .into_iter()
                .map(mathml_to_latex)
                .collect::<Vec<_>>()
                .join(" ");
            format!(r"\sqrt{{{inner}}}")
        }
        "mroot" => {
            let base = children.first().copied().map(mathml_to_latex).unwrap_or_default();
            let idx = children.get(1).copied().map(mathml_to_latex).unwrap_or_default();
            format!(r"\sqrt[{idx}]{{{base}}}")
        }
        "msup" => {
            let base = children.first().copied().map(mathml_to_latex).unwrap_or_default();
            let sup = children.get(1).copied().map(mathml_to_latex).unwrap_or_default();
            format!("{{{base}}}^{{{sup}}}")
        }
        "msub" => {
            let base = children.first().copied().map(mathml_to_latex).unwrap_or_default();
            let sub = children.get(1).copied().map(mathml_to_latex).unwrap_or_default();
            format!("{{{base}}}_{{{sub}}}")
        }
        "msubsup" => {
            let base = children.first().copied().map(mathml_to_latex).unwrap_or_default();
            let sub = children.get(1).copied().map(mathml_to_latex).unwrap_or_default();
            let sup = children.get(2).copied().map(mathml_to_latex).unwrap_or_default();
            format!("{{{base}}}_{{{sub}}}^{{{sup}}}")
        }
        "mover" => {
            let base = children.first().copied().map(mathml_to_latex).unwrap_or_default();
            let over = children.get(1).copied().map(mathml_to_latex).unwrap_or_default();
            let over_trim = over.trim();
            match over_trim {
                "¯" | "‾" | "-" => format!(r"\overline{{{base}}}"),
                "^" | "ˆ" => format!(r"\hat{{{base}}}"),
                "~" | "˜" => format!(r"\tilde{{{base}}}"),
                "→" | r"\to" => format!(r"\vec{{{base}}}"),
                "˙" | "." => format!(r"\dot{{{base}}}"),
                _ => format!(r"\overset{{{over}}}{{{base}}}"),
            }
        }
        "munder" => {
            let base = children.first().copied().map(mathml_to_latex).unwrap_or_default();
            let under = children.get(1).copied().map(mathml_to_latex).unwrap_or_default();
            if matches!(under.trim(), "_" | "‾" | "¯") {
                format!(r"\underline{{{base}}}")
            } else {
                format!(r"\underset{{{under}}}{{{base}}}")
            }
        }
        "munderover" => {
            let base = children.first().copied().map(mathml_to_latex).unwrap_or_default();
            let sub = children.get(1).copied().map(mathml_to_latex).unwrap_or_default();
            let sup = children.get(2).copied().map(mathml_to_latex).unwrap_or_default();
            format!("{{{base}}}_{{{sub}}}^{{{sup}}}")
        }
        "mfenced" => {
            let open = node.attribute("open").unwrap_or("(");
            let close = node.attribute("close").unwrap_or(")");
            let body = children
                .into_iter()
                .map(mathml_to_latex)
                .collect::<Vec<_>>()
                .join(", ");
            let left = if open.is_empty() { "." } else { open };
            let right = if close.is_empty() { "." } else { close };
            format!(r"\left{left} {body} \right{right}")
        }
        "mtable" => {
            let rows = children
                .into_iter()
                .map(mathml_to_latex)
                .collect::<Vec<_>>()
                .join(r" \\ ");
            format!(r"\begin{{matrix}} {rows} \end{{matrix}}")
        }
        "mtr" => children
            .into_iter()
            .map(mathml_to_latex)
            .collect::<Vec<_>>()
            .join(" & "),
        "mtd" => children
            .into_iter()
            .map(mathml_to_latex)
            .collect::<Vec<_>>()
            .join(" "),
        _ => {
            if children.is_empty() {
                translate_symbol(node.text().unwrap_or("").trim())
            } else {
                children
                    .into_iter()
                    .map(mathml_to_latex)
                    .collect::<Vec<_>>()
                    .join(" ")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn converts_academic_mathml_to_latex() {
        let xml = r#"<math xmlns="http://www.w3.org/1998/Math/MathML"><mfrac><msqrt><mi>a</mi><mo>+</mo><mi>b</mi></msqrt><msup><mi>c</mi><mn>2</mn></msup></mfrac></math>"#;
        let doc = roxmltree::Document::parse(xml).unwrap();
        let latex = mathml_to_latex(doc.root_element());
        assert_eq!(latex, r"\frac{\sqrt{a + b}}{{c}^{2}}");
        let font = shared_stix_font().unwrap();
        assert!(render_latex(&latex, font, true, 18.0, 1.0, [0, 0, 0, 255]).is_ok());
    }
}
