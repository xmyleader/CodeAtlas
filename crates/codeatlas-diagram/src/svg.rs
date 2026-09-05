use std::{fmt::Display, fmt::Write as _};

use codeatlas_core::{Diagram, DiagramKind};

use crate::layout::{
    EDGE_LINE_HEIGHT, EdgeLayout, HEADER_KIND_Y, HEADER_TITLE_Y, Layout, NODE_LINE_HEIGHT,
    NodeLayout, OUTER_MARGIN, display_units, is_xml_character,
};

const NODE_FONT_SIZE: i32 = 14;
const EDGE_FONT_SIZE: i32 = 12;
const EDGE_LABEL_UNIT_WIDTH: i32 = 8;

pub(crate) struct RenderedSvg {
    pub(crate) document: String,
    pub(crate) byte_len: usize,
}

struct SvgOutput {
    document: String,
    byte_len: usize,
    maximum: usize,
    overflowed: bool,
}

impl SvgOutput {
    fn new(capacity: usize, maximum: usize) -> Self {
        Self {
            document: String::with_capacity(capacity.min(maximum)),
            byte_len: 0,
            maximum,
            overflowed: false,
        }
    }

    fn push_str(&mut self, value: &str) {
        self.byte_len = self.byte_len.saturating_add(value.len());
        if self.overflowed {
            return;
        }
        if self.byte_len <= self.maximum {
            self.document.push_str(value);
        } else {
            self.document.clear();
            self.overflowed = true;
        }
    }

    fn push(&mut self, character: char) {
        let mut encoded = [0; 4];
        self.push_str(character.encode_utf8(&mut encoded));
    }

    fn finish(self) -> RenderedSvg {
        RenderedSvg {
            document: self.document,
            byte_len: self.byte_len,
        }
    }
}

impl std::fmt::Write for SvgOutput {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        self.push_str(value);
        Ok(())
    }
}

pub(crate) fn render(diagram: &Diagram, layout: &Layout, maximum: usize) -> RenderedSvg {
    let mut output = SvgOutput::new(estimated_capacity(diagram), maximum);
    output.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let width = layout.width;
    let height = layout.height;
    let kind = kind_name(diagram.kind);
    writeln!(
        output,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\" role=\"img\" aria-labelledby=\"diagram-title-0 diagram-description-0\" data-kind=\"{kind}\">",
    )
    .expect("writing SVG to a String cannot fail");
    output.push_str("  <title id=\"diagram-title-0\">");
    write_escaped(&mut output, &diagram.title);
    output.push_str("</title>\n  <desc id=\"diagram-description-0\">CodeAtlas ");
    output.push_str(kind_title(diagram.kind));
    output.push_str(" diagram. Title: ");
    write_escaped(&mut output, &diagram.title);
    output.push_str("</desc>\n");
    output.push_str(
        "  <defs>\n    <marker id=\"arrowhead-0\" markerWidth=\"10\" markerHeight=\"10\" refX=\"9\" refY=\"5\" orient=\"auto\" markerUnits=\"strokeWidth\">\n      <path d=\"M 0 0 L 10 5 L 0 10 Z\" fill=\"#7dd3fc\"/>\n    </marker>\n  </defs>\n",
    );
    writeln!(
        output,
        "  <rect x=\"0\" y=\"0\" width=\"{width}\" height=\"{height}\" fill=\"#080d18\"/>",
    )
    .expect("writing SVG to a String cannot fail");
    write_header(&mut output, diagram, layout.width);

    for (sequence, edge) in layout.edges.iter().enumerate() {
        write_edge(&mut output, diagram, edge, sequence);
    }
    for (sequence, node) in layout.nodes.iter().enumerate() {
        write_node(&mut output, diagram, node, sequence);
    }

    output.push_str("</svg>\n");
    output.finish()
}

fn write_header(output: &mut SvgOutput, diagram: &Diagram, width: i32) {
    let margin = OUTER_MARGIN;
    let title_y = HEADER_TITLE_Y;
    write!(
        output,
        "  <text x=\"{margin}\" y=\"{title_y}\" fill=\"#f8fafc\" font-family=\"ui-monospace, monospace\" font-size=\"18\" font-weight=\"700\">",
    )
    .expect("writing SVG to a String cannot fail");
    write_escaped(output, &diagram.title);
    output.push_str("</text>\n");
    let kind_y = HEADER_KIND_Y;
    let kind = kind_name(diagram.kind).to_ascii_uppercase();
    writeln!(
        output,
        "  <text x=\"{margin}\" y=\"{kind_y}\" fill=\"#7dd3fc\" font-family=\"ui-monospace, monospace\" font-size=\"12\" font-weight=\"700\">{kind}</text>",
    )
    .expect("writing SVG to a String cannot fail");
    let divider_end = width - OUTER_MARGIN;
    writeln!(
        output,
        "  <path d=\"M {margin} 76 L {divider_end} 76\" fill=\"none\" stroke=\"#263248\" stroke-width=\"1\"/>",
    )
    .expect("writing SVG to a String cannot fail");
}

fn write_edge(
    output: &mut SvgOutput,
    diagram: &Diagram,
    edge_layout: &EdgeLayout,
    sequence: usize,
) {
    let edge = &diagram.edges[edge_layout.diagram_index];
    let source = &diagram.nodes[edge_layout.source_index];
    let target = &diagram.nodes[edge_layout.target_index];
    let class = if edge_layout.dashed {
        "edge cycle-edge"
    } else {
        "edge"
    };
    write!(
        output,
        "  <g id=\"edge-{sequence}\" class=\"{class}\" data-claim-ids=\"",
    )
    .expect("writing SVG to a String cannot fail");
    write_ids(output, &edge.claim_ids);
    output.push_str("\" data-evidence-ids=\"");
    write_ids(output, &edge.evidence_ids);
    output.push_str("\">\n    <title>");
    write_escaped(output, &source.label);
    output.push_str(" -&gt; ");
    write_escaped(output, &target.label);
    if edge
        .label
        .chars()
        .any(|character| is_xml_character(character) && !character.is_whitespace())
    {
        output.push_str(": ");
        write_escaped(output, &edge.label);
    }
    output.push_str("</title>\n");

    let path = &edge_layout.path;
    let start_x = path.start.x;
    let start_y = path.start.y;
    let control_1_x = path.control_1.x;
    let control_1_y = path.control_1.y;
    let control_2_x = path.control_2.x;
    let control_2_y = path.control_2.y;
    let end_x = path.end.x;
    let end_y = path.end.y;
    write!(
        output,
        "    <path d=\"M {start_x} {start_y} C {control_1_x} {control_1_y} {control_2_x} {control_2_y} {end_x} {end_y}\" fill=\"none\" stroke=\"#7dd3fc\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\" marker-end=\"url(#arrowhead-0)\"",
    )
    .expect("writing SVG to a String cannot fail");
    if edge_layout.dashed {
        output.push_str(" stroke-dasharray=\"8 7\"");
    }
    output.push_str("/>\n");

    if edge
        .label
        .chars()
        .any(|character| is_xml_character(character) && !character.is_whitespace())
    {
        write_edge_label(output, edge_layout);
    }
    output.push_str("  </g>\n");
}

fn write_edge_label(output: &mut SvgOutput, edge: &EdgeLayout) {
    let maximum_units = edge
        .label_lines
        .iter()
        .map(|line| display_units(line))
        .max()
        .unwrap_or(0);
    let box_width = to_i32(maximum_units) * EDGE_LABEL_UNIT_WIDTH + 16;
    let box_height = to_i32(edge.label_lines.len()) * EDGE_LINE_HEIGHT + 10;
    let box_x = edge.label_position.x - box_width / 2;
    let box_y = edge.label_position.y - box_height / 2;
    writeln!(
        output,
        "    <rect x=\"{box_x}\" y=\"{box_y}\" width=\"{box_width}\" height=\"{box_height}\" rx=\"5\" fill=\"#111a2b\" stroke=\"#334155\" stroke-width=\"1\"/>",
    )
    .expect("writing SVG to a String cannot fail");

    let first_baseline = edge.label_position.y
        - (to_i32(edge.label_lines.len().saturating_sub(1)) * EDGE_LINE_HEIGHT) / 2
        + 4;
    let label_x = edge.label_position.x;
    writeln!(
        output,
        "    <text x=\"{label_x}\" y=\"{first_baseline}\" text-anchor=\"middle\" fill=\"#bae6fd\" font-family=\"ui-monospace, monospace\" font-size=\"{EDGE_FONT_SIZE}\" font-weight=\"600\">",
    )
    .expect("writing SVG to a String cannot fail");
    write_tspans(
        output,
        &edge.label_lines,
        edge.label_position.x,
        first_baseline,
        EDGE_LINE_HEIGHT,
        6,
    );
    output.push_str("    </text>\n");
}

fn write_node(
    output: &mut SvgOutput,
    diagram: &Diagram,
    node_layout: &NodeLayout,
    sequence: usize,
) {
    let node = &diagram.nodes[node_layout.diagram_index];
    let class = if node_layout.focused {
        "node focus-node"
    } else {
        "node"
    };
    write!(
        output,
        "  <g id=\"node-{sequence}\" class=\"{class}\" data-claim-ids=\"",
    )
    .expect("writing SVG to a String cannot fail");
    write_ids(output, &node.claim_ids);
    output.push_str("\" data-evidence-ids=\"");
    write_ids(output, &node.evidence_ids);
    output.push_str("\">\n    <title>");
    write_escaped(output, &node.label);
    output.push_str("</title>\n");

    let rect = node_layout.rect;
    let (fill, stroke) = if node_layout.focused {
        ("#16243a", "#fbbf24")
    } else {
        ("#111a2b", "#52647f")
    };
    let x = rect.x;
    let y = rect.y;
    let width = rect.width;
    let height = rect.height;
    writeln!(
        output,
        "    <rect x=\"{x}\" y=\"{y}\" width=\"{width}\" height=\"{height}\" rx=\"12\" fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"2\"/>",
    )
    .expect("writing SVG to a String cannot fail");

    let first_baseline = rect.center_y()
        - (to_i32(node_layout.lines.len().saturating_sub(1)) * NODE_LINE_HEIGHT) / 2
        + 5;
    let center_x = rect.center_x();
    writeln!(
        output,
        "    <text x=\"{center_x}\" y=\"{first_baseline}\" text-anchor=\"middle\" fill=\"#e2e8f0\" font-family=\"ui-monospace, monospace\" font-size=\"{NODE_FONT_SIZE}\" font-weight=\"650\">",
    )
    .expect("writing SVG to a String cannot fail");
    write_tspans(
        output,
        &node_layout.lines,
        rect.center_x(),
        first_baseline,
        NODE_LINE_HEIGHT,
        6,
    );
    output.push_str("    </text>\n  </g>\n");
}

fn write_tspans(
    output: &mut SvgOutput,
    lines: &[String],
    x: i32,
    first_y: i32,
    line_height: i32,
    indentation: usize,
) {
    let spaces = " ".repeat(indentation);
    for (line_index, line) in lines.iter().enumerate() {
        let y = first_y + to_i32(line_index) * line_height;
        write!(output, "{spaces}<tspan x=\"{x}\" y=\"{y}\">")
            .expect("writing SVG to a String cannot fail");
        write_escaped(output, line);
        output.push_str("</tspan>\n");
    }
}

fn write_ids<T: Display>(output: &mut SvgOutput, ids: &[T]) {
    for (index, id) in ids.iter().enumerate() {
        if index != 0 {
            output.push(' ');
        }
        write_escaped(output, &id.to_string());
    }
}

fn write_escaped(output: &mut SvgOutput, value: &str) {
    for character in value.chars().filter(|value| is_xml_character(*value)) {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ => output.push(character),
        }
    }
}

const fn kind_name(kind: DiagramKind) -> &'static str {
    match kind {
        DiagramKind::Architecture => "architecture",
        DiagramKind::Flow => "flow",
        DiagramKind::Relationship => "relationship",
    }
}

const fn kind_title(kind: DiagramKind) -> &'static str {
    match kind {
        DiagramKind::Architecture => "Architecture",
        DiagramKind::Flow => "Flow",
        DiagramKind::Relationship => "Relationship",
    }
}

fn estimated_capacity(diagram: &Diagram) -> usize {
    2_048 + diagram.nodes.len() * 512 + diagram.edges.len() * 640
}

fn to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}
