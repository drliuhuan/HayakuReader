use crate::theme::ThemeMode;
use egui::{Color32, FontId, Stroke, TextFormat, text::LayoutJob};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeLang {
    XmlHtml,
    Markdown,
    Json,
    Code,
    Plain,
}

pub fn detect_lang(hint: &str) -> CodeLang {
    let lower = hint.trim().to_ascii_lowercase();
    match lower.as_str() {
        "xml" | "html" | "htm" | "xhtml" | "svg" | "mathml" => CodeLang::XmlHtml,
        "md" | "markdown" => CodeLang::Markdown,
        "json" | "jsonc" => CodeLang::Json,
        "" | "text" | "txt" | "plain" => CodeLang::Plain,
        _ => CodeLang::Code,
    }
}

struct SyntaxColors {
    plain: Color32,
    muted: Color32,
    keyword: Color32,
    string: Color32,
    number: Color32,
    comment: Color32,
    tag: Color32,
    attr: Color32,
    punct: Color32,
}

impl SyntaxColors {
    fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self {
                plain: Color32::from_rgb(0xe8, 0xec, 0xe4),
                muted: Color32::from_rgb(0x9a, 0xa8, 0x9e),
                keyword: Color32::from_rgb(0xd8, 0x9b, 0xff),
                string: Color32::from_rgb(0xa3, 0xd9, 0x9c),
                number: Color32::from_rgb(0xf2, 0xb8, 0x80),
                comment: Color32::from_rgb(0x7c, 0x8c, 0x82),
                tag: Color32::from_rgb(0x7d, 0xc4, 0xff),
                attr: Color32::from_rgb(0xff, 0xcb, 0x8b),
                punct: Color32::from_rgb(0x88, 0xd1, 0xc7),
            },
            ThemeMode::Light => Self {
                plain: Color32::from_rgb(0x23, 0x2b, 0x26),
                muted: Color32::from_rgb(0x64, 0x70, 0x69),
                keyword: Color32::from_rgb(0x8a, 0x2b, 0x8a),
                string: Color32::from_rgb(0x1f, 0x73, 0x3b),
                number: Color32::from_rgb(0xb8, 0x52, 0x0a),
                comment: Color32::from_rgb(0x6c, 0x78, 0x70),
                tag: Color32::from_rgb(0x0b, 0x5c, 0xab),
                attr: Color32::from_rgb(0x9b, 0x42, 0x16),
                punct: Color32::from_rgb(0x1b, 0x6d, 0x7a),
            },
        }
    }
}

fn push_span(job: &mut LayoutJob, text: &str, color: Color32, font_size: f32, italic: bool) {
    if text.is_empty() {
        return;
    }
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: FontId::monospace(font_size),
            color,
            italics: italic,
            underline: Stroke::NONE,
            strikethrough: Stroke::NONE,
            ..Default::default()
        },
    );
}

pub fn highlight_line(line: &str, lang: CodeLang, mode: ThemeMode, font_size: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let colors = SyntaxColors::for_mode(mode);
    match lang {
        CodeLang::XmlHtml => highlight_xml_line(line, &colors, font_size, &mut job),
        CodeLang::Markdown => highlight_md_line(line, &colors, font_size, &mut job),
        CodeLang::Json => highlight_json_line(line, &colors, font_size, &mut job),
        CodeLang::Code => highlight_code_line(line, &colors, font_size, &mut job),
        CodeLang::Plain => push_span(&mut job, line, colors.plain, font_size, false),
    }
    job
}

pub fn highlight_block(code: &str, lang: CodeLang, mode: ThemeMode, font_size: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let colors = SyntaxColors::for_mode(mode);
    for (idx, line) in code.split('\n').enumerate() {
        if idx > 0 {
            push_span(&mut job, "\n", colors.plain, font_size, false);
        }
        match lang {
            CodeLang::XmlHtml => highlight_xml_line(line, &colors, font_size, &mut job),
            CodeLang::Markdown => highlight_md_line(line, &colors, font_size, &mut job),
            CodeLang::Json => highlight_json_line(line, &colors, font_size, &mut job),
            CodeLang::Code => highlight_code_line(line, &colors, font_size, &mut job),
            CodeLang::Plain => push_span(&mut job, line, colors.plain, font_size, false),
        }
    }
    job
}

fn highlight_xml_line(line: &str, c: &SyntaxColors, fsize: f32, job: &mut LayoutJob) {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if line[i..].starts_with("<!--") {
            let end = line[i..].find("-->").map_or(line.len(), |e| i + e + 3);
            push_span(job, &line[i..end], c.comment, fsize, true);
            i = end;
        } else if bytes[i] == b'<' {
            let end_tag = line[i..].find('>').map_or(line.len(), |e| i + e + 1);
            highlight_xml_tag(&line[i..end_tag], c, fsize, job);
            i = end_tag;
        } else if bytes[i] == b'&'
            && let Some(semi) = line[i..].find(';').filter(|&s| s <= 14)
        {
            push_span(job, &line[i..i + semi + 1], c.number, fsize, false);
            i += semi + 1;
        } else {
            let next = line[i..].find(['<', '&']).map_or(line.len(), |n| i + n);
            let step = if next == i {
                line[i..].chars().next().map_or(1, char::len_utf8) + i
            } else {
                next
            };
            push_span(job, &line[i..step], c.plain, fsize, false);
            i = step;
        }
    }
}

fn highlight_xml_tag(tag_slice: &str, c: &SyntaxColors, fsize: f32, job: &mut LayoutJob) {
    let bytes = tag_slice.as_bytes();
    let mut i = 0usize;
    // "<", "</", "<?", "<!"
    let prefix_len = if tag_slice.starts_with("</")
        || tag_slice.starts_with("<?")
        || tag_slice.starts_with("<!")
    {
        2.min(bytes.len())
    } else {
        1.min(bytes.len())
    };
    push_span(job, &tag_slice[..prefix_len], c.punct, fsize, false);
    i += prefix_len;

    // Tag name
    let name_start = i;
    while i < bytes.len()
        && !bytes[i].is_ascii_whitespace()
        && !matches!(bytes[i], b'>' | b'/' | b'?')
    {
        i += 1;
    }
    push_span(job, &tag_slice[name_start..i], c.tag, fsize, false);

    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            let ws_start = i;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            push_span(job, &tag_slice[ws_start..i], c.plain, fsize, false);
        } else if b == b'"' || b == b'\'' {
            let q = b;
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != q {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            push_span(job, &tag_slice[start..i], c.string, fsize, false);
        } else if matches!(b, b'=' | b'/' | b'>' | b'?') {
            push_span(job, &tag_slice[i..i + 1], c.punct, fsize, false);
            i += 1;
        } else {
            let attr_start = i;
            while i < bytes.len()
                && !bytes[i].is_ascii_whitespace()
                && !matches!(bytes[i], b'=' | b'/' | b'>' | b'"' | b'\'')
            {
                i += 1;
            }
            push_span(job, &tag_slice[attr_start..i], c.attr, fsize, false);
        }
    }
}

fn highlight_md_line(line: &str, c: &SyntaxColors, fsize: f32, job: &mut LayoutJob) {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        push_span(job, line, c.tag, fsize, false);
        return;
    }
    if trimmed.starts_with("```") || trimmed.starts_with("---") || trimmed.starts_with("***") {
        push_span(job, line, c.comment, fsize, false);
        return;
    }
    if trimmed.starts_with('>') {
        push_span(job, line, c.muted, fsize, true);
        return;
    }
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'`'
            && let Some(end) = line[i + 1..].find('`')
        {
            let total_end = i + 1 + end + 1;
            push_span(job, &line[i..total_end], c.string, fsize, false);
            i = total_end;
        } else if bytes[i] == b'$'
            && let Some(end) = line[i + 1..].find('$')
        {
            let total_end = i + 1 + end + 1;
            push_span(job, &line[i..total_end], c.number, fsize, false);
            i = total_end;
        } else if bytes[i] == b'['
            && let Some(close_br) = line[i..].find("](")
            && let Some(close_par) = line[i + close_br + 2..].find(')')
        {
            let total_end = i + close_br + 2 + close_par + 1;
            push_span(job, &line[i..total_end], c.attr, fsize, false);
            i = total_end;
        } else {
            let char_len = line[i..].chars().next().map_or(1, char::len_utf8);
            let next = line[i + char_len..]
                .find(['`', '$', '['])
                .map_or(line.len(), |n| i + char_len + n);
            push_span(job, &line[i..next], c.plain, fsize, false);
            i = next;
        }
    }
}

fn highlight_json_line(line: &str, c: &SyntaxColors, fsize: f32, job: &mut LayoutJob) {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    let esc_len = line[i + 1..].chars().next().map_or(1, char::len_utf8);
                    i += 1 + esc_len;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let after_ws = line[i..].trim_start();
            let col = if after_ws.starts_with(':') {
                c.tag
            } else {
                c.string
            };
            push_span(job, &line[start..i], col, fsize, false);
        } else if b.is_ascii_digit()
            || (b == b'-' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit() || matches!(bytes[i], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
                i += 1;
            }
            push_span(job, &line[start..i], c.number, fsize, false);
        } else if b.is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                i += 1;
            }
            let word = &line[start..i];
            let col = if matches!(word, "true" | "false" | "null") {
                c.keyword
            } else {
                c.plain
            };
            push_span(job, word, col, fsize, false);
        } else {
            let step = i + line[i..].chars().next().map_or(1, char::len_utf8);
            let col = if matches!(b, b'{' | b'}' | b'[' | b']' | b':' | b',') {
                c.punct
            } else {
                c.plain
            };
            push_span(job, &line[i..step], col, fsize, false);
            i = step;
        }
    }
}

fn highlight_code_line(line: &str, c: &SyntaxColors, fsize: f32, job: &mut LayoutJob) {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let rest = &line[i..];
        if rest.starts_with("//") || rest.starts_with('#') {
            push_span(job, rest, c.comment, fsize, true);
            break;
        }
        let b = bytes[i];
        if b == b'"' || b == b'\'' {
            let q = b;
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    let esc_len = line[i + 1..].chars().next().map_or(1, char::len_utf8);
                    i += 1 + esc_len;
                    continue;
                }
                if bytes[i] == q {
                    i += 1;
                    break;
                }
                i += 1;
            }
            push_span(job, &line[start..i], c.string, fsize, false);
        } else if b.is_ascii_digit() {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'_'))
            {
                i += 1;
            }
            push_span(job, &line[start..i], c.number, fsize, false);
        } else if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[start..i];
            let col = if is_code_keyword(word) {
                c.keyword
            } else if word.chars().next().is_some_and(char::is_uppercase) {
                c.attr
            } else {
                c.plain
            };
            push_span(job, word, col, fsize, false);
        } else {
            let step = i + line[i..].chars().next().map_or(1, char::len_utf8);
            let col = if matches!(
                b,
                b'{' | b'}' | b'(' | b')' | b'[' | b']' | b';' | b':' | b'=' | b'<' | b'>'
            ) {
                c.punct
            } else {
                c.plain
            };
            push_span(job, &line[i..step], col, fsize, false);
            i = step;
        }
    }
}

fn is_code_keyword(word: &str) -> bool {
    matches!(
        word,
        "fn" | "let"
            | "mut"
            | "const"
            | "static"
            | "pub"
            | "struct"
            | "enum"
            | "impl"
            | "trait"
            | "match"
            | "if"
            | "else"
            | "for"
            | "while"
            | "loop"
            | "return"
            | "break"
            | "continue"
            | "use"
            | "mod"
            | "crate"
            | "async"
            | "await"
            | "function"
            | "var"
            | "class"
            | "def"
            | "import"
            | "from"
            | "export"
            | "default"
            | "true"
            | "false"
            | "null"
            | "None"
            | "Some"
            | "Ok"
            | "Err"
            | "self"
            | "Self"
            | "in"
            | "as"
            | "where"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_xml_tags_attributes_and_strings() {
        let job = highlight_line(
            r#"  <p id="sec1" class='lead'>Hello &amp; world</p> <!-- note -->"#,
            CodeLang::XmlHtml,
            ThemeMode::Light,
            15.0,
        );
        assert!(job.sections.len() >= 7);
        let colors: std::collections::HashSet<Color32> =
            job.sections.iter().map(|s| s.format.color).collect();
        assert!(colors.len() >= 4);
    }

    #[test]
    fn highlights_code_and_markdown() {
        let code_job = highlight_block(
            "fn main() {\n    let x = \"hi\"; // ok\n}",
            CodeLang::Code,
            ThemeMode::Dark,
            14.0,
        );
        let md_job = highlight_line(
            "# Title with `code` and $x^2$",
            CodeLang::Markdown,
            ThemeMode::Dark,
            14.0,
        );
        assert!(code_job.sections.len() >= 6);
        assert!(md_job.sections.len() == 1);
    }

    #[test]
    fn highlights_multibyte_utf8_markdown_json_and_code_without_panicking() {
        let md_job = highlight_line(
            "先说结论：`code` 与 $x^2$ 和 [链接](https://example.com)",
            CodeLang::Markdown,
            ThemeMode::Light,
            15.0,
        );
        assert!(md_job.sections.len() >= 5);

        let json_job = highlight_line(
            r#"{"key": "先\中\"后"}"#,
            CodeLang::Json,
            ThemeMode::Dark,
            14.0,
        );
        assert!(json_job.sections.len() >= 3);

        let code_job = highlight_line(
            r#"let s = "中文\测试";"#,
            CodeLang::Code,
            ThemeMode::Light,
            14.0,
        );
        assert!(code_job.sections.len() >= 4);
    }
}
