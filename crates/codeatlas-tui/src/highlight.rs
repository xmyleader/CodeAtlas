use std::{path::Path, sync::OnceLock};

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, Style as SyntectStyle, ThemeSet},
    parsing::{SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};

const THEME_NAME: &str = "base16-ocean.dark";

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEMES: OnceLock<ThemeSet> = OnceLock::new();

/// Highlights `source` according to `path`, preserving lexer state across lines.
///
/// Line endings are structural and are not included in the returned display
/// spans. Unsupported paths and highlighting failures use `fallback`.
pub(crate) fn highlight_source_lines(
    path: &Path,
    source: &str,
    fallback: Style,
) -> Vec<Vec<Span<'static>>> {
    let syntaxes = SYNTAXES.get_or_init(two_face::syntax::extra_newlines);
    let themes = THEMES.get_or_init(ThemeSet::load_defaults);
    let Some(syntax) = syntax_for_path(path, syntaxes) else {
        return fallback_lines(source, fallback);
    };
    let Some(theme) = themes.themes.get(THEME_NAME) else {
        return fallback_lines(source, fallback);
    };

    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(source) {
        let Ok(regions) = highlighter.highlight_line(line, syntaxes) else {
            return fallback_lines(source, fallback);
        };
        let display_len = line_without_ending(line).len();
        let mut remaining = display_len;
        let mut spans = Vec::new();
        for (style, text) in regions {
            if remaining == 0 {
                break;
            }
            let take = text.len().min(remaining);
            spans.push(Span::styled(text[..take].to_owned(), ratatui_style(style)));
            remaining -= take;
        }
        lines.push(spans);
    }
    lines
}

fn fallback_lines(source: &str, fallback: Style) -> Vec<Vec<Span<'static>>> {
    LinesWithEndings::from(source)
        .map(|line| vec![Span::styled(line_without_ending(line).to_owned(), fallback)])
        .collect()
}

fn line_without_ending(line: &str) -> &str {
    line.strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .unwrap_or(line)
}

fn syntax_for_path<'a>(path: &Path, syntaxes: &'a SyntaxSet) -> Option<&'a SyntaxReference> {
    let extension = path.extension()?.to_str()?;
    let canonical_extension = match extension.to_ascii_lowercase().as_str() {
        "rs" => "rs",
        "py" | "pyi" => "py",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
        "java" => "java",
        "js" | "jsx" | "mjs" | "cjs" => "js",
        "ts" | "tsx" | "mts" | "cts" => "ts",
        "go" => "go",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "sh" | "bash" | "zsh" => "sh",
        "html" | "htm" => "html",
        "css" => "css",
        "sql" => "sql",
        _ => return None,
    };

    syntaxes.find_syntax_by_extension(canonical_extension)
}

fn ratatui_style(style: SyntectStyle) -> Style {
    let mut result = Style::default().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));
    if style.font_style.contains(FontStyle::BOLD) {
        result = result.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        result = result.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        result = result.add_modifier(Modifier::UNDERLINED);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_language_extensions() {
        let syntaxes = two_face::syntax::extra_newlines();
        let extensions = [
            "rs", "py", "pyi", "c", "h", "cc", "cpp", "cxx", "hh", "hpp", "hxx", "java", "js",
            "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "go", "json", "toml", "yaml", "yml",
            "sh", "bash", "zsh", "html", "htm", "css", "sql",
        ];

        for extension in extensions {
            let path = format!("example.{extension}");
            assert!(
                syntax_for_path(Path::new(&path), &syntaxes).is_some(),
                "expected recognition for .{extension}"
            );
        }
        assert!(syntax_for_path(Path::new("EXAMPLE.RS"), &syntaxes).is_some());
    }

    #[test]
    fn rust_and_python_receive_multiple_token_styles() {
        for (path, source) in [
            ("main.rs", "fn main() { let answer = 42; }\n"),
            (
                "main.py",
                "def greet(name):\n    return f\"Hello, {name}\"\n",
            ),
        ] {
            let spans = highlight_source_lines(Path::new(path), source, Style::default())
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            let first_style = spans.first().expect("highlighting should emit spans").style;
            assert!(
                spans.iter().any(|span| span.style != first_style),
                "expected multiple token styles for {path}"
            );
        }
    }

    #[test]
    fn unknown_path_uses_fallback_spans() {
        let fallback = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let source = "not syntax highlighted\n";
        let lines = highlight_source_lines(Path::new("README.unknown"), source, fallback);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), 1);
        assert_eq!(lines[0][0].content, "not syntax highlighted");
        assert_eq!(lines[0][0].style, fallback);
    }

    #[test]
    fn preserves_source_text_with_line_endings_removed_for_display() {
        let source = "fn main() {\r\n\tprintln!(\"hello\");\r\n}\nno_newline";
        let lines = highlight_source_lines(Path::new("main.rs"), source, Style::default());
        let reconstructed = lines
            .iter()
            .map(|spans| {
                spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            reconstructed,
            ["fn main() {", "\tprintln!(\"hello\");", "}", "no_newline"]
        );
    }

    #[test]
    fn preserves_multiline_lexer_state() {
        let lines = highlight_source_lines(
            Path::new("main.rs"),
            "/* opening\nstill a comment */\nfn main() {}",
            Style::default(),
        );
        let opening = lines[0].first().expect("opening comment span").style.fg;
        let continuation = lines[1]
            .first()
            .expect("continuation comment span")
            .style
            .fg;

        assert_eq!(opening, continuation);
        assert_ne!(continuation, lines[2].first().expect("code span").style.fg);
    }
}
