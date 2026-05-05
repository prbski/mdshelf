use std::path::PathBuf;

use comrak::{
    Anchorizer, Arena, Options,
    nodes::{AstNode, NodeHtmlBlock, NodeValue},
    options::{Extension, Parse, Plugins, Render},
    plugins::syntect::SyntectAdapter,
};
use serde::Serialize;

use crate::render::highlight::build_syntax_css;

#[derive(Debug, Clone, Serialize)]
pub struct Heading {
    pub level: u8,
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Rendered {
    pub html: String,
    pub headings: Vec<Heading>,
}

/// Information needed to rewrite intra-site Markdown links to clean URLs.
#[derive(Debug, Clone)]
pub struct LinkCtx {
    /// URL prefix the page lives under (e.g. "/notes" or "/").
    pub mount: String,
    /// Directory of the source file relative to the site root (e.g. "guide").
    pub current_dir: PathBuf,
}

pub struct MarkdownRenderer {
    options: Options<'static>,
    syntect: SyntectAdapter,
    syntax_css: String,
}

impl MarkdownRenderer {
    pub fn new() -> Self {
        let mut extension = Extension::default();
        extension.strikethrough = true;
        extension.tagfilter = false;
        extension.table = true;
        extension.autolink = true;
        extension.tasklist = true;
        extension.superscript = true;
        extension.footnotes = true;
        extension.description_lists = true;
        extension.multiline_block_quotes = true;
        extension.math_dollars = true;
        extension.math_code = true;
        extension.header_id_prefix = Some(String::new());

        let mut render = Render::default();
        render.r#unsafe = true;
        render.escape = false;
        render.hardbreaks = false;

        let parse = Parse::default();

        let options = Options {
            extension,
            parse,
            render,
        };

        Self {
            options,
            syntect: SyntectAdapter::new(None),
            syntax_css: build_syntax_css(),
        }
    }

    pub fn render(&self, source: &str, ctx: &LinkCtx) -> Rendered {
        let arena = Arena::new();
        let root = comrak::parse_document(&arena, source, &self.options);

        let mut anchorizer = Anchorizer::new();
        let mut headings: Vec<Heading> = Vec::new();
        for node in root.descendants() {
            self.process_node(node, ctx, &mut anchorizer, &mut headings);
        }

        let mut buf = String::with_capacity(source.len() * 2);
        let mut plugins = Plugins::default();
        plugins.render.codefence_syntax_highlighter = Some(&self.syntect);
        comrak::format_html_with_plugins(root, &self.options, &mut buf, &plugins)
            .expect("comrak HTML formatting cannot fail when writing to String");

        Rendered {
            html: buf,
            headings,
        }
    }

    pub fn syntax_css(&self) -> &str {
        &self.syntax_css
    }

    fn process_node<'a>(
        &self,
        node: &'a AstNode<'a>,
        ctx: &LinkCtx,
        anchorizer: &mut Anchorizer,
        headings: &mut Vec<Heading>,
    ) {
        if let NodeValue::Heading(h) = &node.data.borrow().value {
            let text = collect_text(node);
            let id = anchorizer.anchorize(&text);
            headings.push(Heading {
                level: h.level,
                id,
                text,
            });
        }

        let mermaid_literal = match &node.data.borrow().value {
            NodeValue::CodeBlock(code) if code.info.trim() == "mermaid" => {
                Some(code.literal.clone())
            }
            _ => None,
        };
        if let Some(literal) = mermaid_literal {
            node.data.borrow_mut().value = NodeValue::HtmlBlock(NodeHtmlBlock {
                block_type: 6,
                literal: format!("<div class=\"mermaid\">{}</div>\n", html_escape(&literal)),
            });
            return;
        }

        match &mut node.data.borrow_mut().value {
            NodeValue::Link(link) => {
                if let Some(rewritten) = rewrite_link(&link.url, ctx) {
                    link.url = rewritten;
                }
            }
            NodeValue::Image(img) => {
                if let Some(rewritten) = rewrite_asset(&img.url, ctx) {
                    img.url = rewritten;
                }
            }
            _ => {}
        }
    }
}

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new()
    }
}

fn collect_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut out = String::new();
    for desc in node.descendants() {
        match &desc.data.borrow().value {
            NodeValue::Text(text) => out.push_str(text),
            NodeValue::Code(code) => out.push_str(&code.literal),
            _ => {}
        }
    }
    out.trim().to_string()
}

fn rewrite_link(url: &str, ctx: &LinkCtx) -> Option<String> {
    if has_scheme(url) || url.starts_with('#') || url.starts_with("mailto:") {
        return None;
    }

    let (path_part, suffix) = split_suffix(url);
    let target_path = if path_part.starts_with('/') {
        path_part.trim_start_matches('/').to_string()
    } else {
        join_relative(&ctx.current_dir, path_part)
    };

    let lower = target_path.to_ascii_lowercase();
    if !lower.ends_with(".md") {
        return None;
    }

    let trimmed = strip_md_extension(&target_path);
    let final_path = strip_index_segment(&trimmed);
    let mount = if ctx.mount == "/" { "" } else { &ctx.mount };
    let url = if final_path.is_empty() {
        if mount.is_empty() {
            "/".to_string()
        } else {
            mount.to_string()
        }
    } else {
        format!("{}/{}", mount, final_path)
    };
    Some(format!("{}{}", url, suffix))
}

fn rewrite_asset(url: &str, ctx: &LinkCtx) -> Option<String> {
    if has_scheme(url) || url.starts_with('#') || url.starts_with('/') {
        return None;
    }
    let (path_part, suffix) = split_suffix(url);
    let target_path = join_relative(&ctx.current_dir, path_part);
    let mount = if ctx.mount == "/" { "" } else { &ctx.mount };
    Some(format!("{}/{}{}", mount, target_path, suffix))
}

fn has_scheme(url: &str) -> bool {
    url.contains("://") || url.starts_with("//")
}

fn split_suffix(url: &str) -> (&str, &str) {
    let split_at = url.find(['?', '#']).unwrap_or(url.len());
    (&url[..split_at], &url[split_at..])
}

fn strip_md_extension(path: &str) -> String {
    if let Some(stripped) = path
        .rsplit_once('.')
        .filter(|(_, ext)| ext.eq_ignore_ascii_case("md"))
        .map(|(prefix, _)| prefix)
    {
        stripped.to_string()
    } else {
        path.to_string()
    }
}

fn strip_index_segment(path: &str) -> String {
    if path.eq_ignore_ascii_case("index") {
        return String::new();
    }
    if let Some(prefix) = path.strip_suffix("/index").or_else(|| {
        if path.ends_with("/Index") || path.ends_with("/INDEX") {
            Some(&path[..path.len() - "/index".len()])
        } else {
            None
        }
    }) {
        prefix.to_string()
    } else {
        path.to_string()
    }
}

fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn join_relative(base: &std::path::Path, rel: &str) -> String {
    let mut parts: Vec<String> = base
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();

    for segment in rel.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other.to_string()),
        }
    }
    parts.join("/")
}
