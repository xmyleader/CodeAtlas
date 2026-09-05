//! Deterministic, dependency-free SVG rendering for `CodeAtlas` diagrams.

mod error;
mod layout;
mod svg;

use std::collections::BTreeMap;

use codeatlas_core::Diagram;

pub use error::DiagramRenderError;

const MAX_NODES: usize = 32;
const MAX_EDGES: usize = 64;
const MAX_TITLE_BYTES: usize = 256;
const MAX_LABEL_BYTES: usize = 128;
const MAX_DIMENSION: i32 = 4_096;
const MAX_SVG_BYTES: usize = 1024 * 1024;

/// Renders a diagram as a self-contained, deterministic SVG document.
///
/// The renderer does not invoke external programs or load external resources.
/// All coordinates are integral and all model-provided text is XML-escaped.
///
/// # Errors
///
/// Returns [`DiagramRenderError`] when an input or output limit is exceeded,
/// node IDs are ambiguous, an edge endpoint is unknown, or the resulting SVG
/// would exceed the configured canvas or byte-size limits.
pub fn render_svg(diagram: &Diagram) -> Result<Vec<u8>, DiagramRenderError> {
    validate_limits(diagram)?;
    let endpoints = resolve_endpoints(diagram)?;
    let layout = layout::build(diagram, &endpoints);

    if layout.width > MAX_DIMENSION {
        return Err(DiagramRenderError::WidthLimitExceeded {
            actual: usize::try_from(layout.width).unwrap_or(usize::MAX),
            maximum: usize::try_from(MAX_DIMENSION).unwrap_or(usize::MAX),
        });
    }
    if layout.height > MAX_DIMENSION {
        return Err(DiagramRenderError::HeightLimitExceeded {
            actual: usize::try_from(layout.height).unwrap_or(usize::MAX),
            maximum: usize::try_from(MAX_DIMENSION).unwrap_or(usize::MAX),
        });
    }

    let rendered = svg::render(diagram, &layout, MAX_SVG_BYTES);
    if rendered.byte_len > MAX_SVG_BYTES {
        return Err(DiagramRenderError::SvgSizeLimitExceeded {
            actual: rendered.byte_len,
            maximum: MAX_SVG_BYTES,
        });
    }

    Ok(rendered.document.into_bytes())
}

fn validate_limits(diagram: &Diagram) -> Result<(), DiagramRenderError> {
    if diagram.nodes.len() > MAX_NODES {
        return Err(DiagramRenderError::TooManyNodes {
            actual: diagram.nodes.len(),
            maximum: MAX_NODES,
        });
    }
    if diagram.edges.len() > MAX_EDGES {
        return Err(DiagramRenderError::TooManyEdges {
            actual: diagram.edges.len(),
            maximum: MAX_EDGES,
        });
    }
    if diagram.title.len() > MAX_TITLE_BYTES {
        return Err(DiagramRenderError::TitleTooLong {
            actual: diagram.title.len(),
            maximum: MAX_TITLE_BYTES,
        });
    }

    for (node_index, node) in diagram.nodes.iter().enumerate() {
        if node.label.len() > MAX_LABEL_BYTES {
            return Err(DiagramRenderError::NodeLabelTooLong {
                node_index,
                actual: node.label.len(),
                maximum: MAX_LABEL_BYTES,
            });
        }
    }
    for (edge_index, edge) in diagram.edges.iter().enumerate() {
        if edge.label.len() > MAX_LABEL_BYTES {
            return Err(DiagramRenderError::EdgeLabelTooLong {
                edge_index,
                actual: edge.label.len(),
                maximum: MAX_LABEL_BYTES,
            });
        }
    }

    Ok(())
}

fn resolve_endpoints(diagram: &Diagram) -> Result<Vec<(usize, usize)>, DiagramRenderError> {
    let mut node_indices = BTreeMap::new();
    for (node_index, node) in diagram.nodes.iter().enumerate() {
        if let Some(first_node_index) = node_indices.insert(node.id.as_str(), node_index) {
            return Err(DiagramRenderError::DuplicateNodeId {
                first_node_index,
                duplicate_node_index: node_index,
            });
        }
    }

    diagram
        .edges
        .iter()
        .enumerate()
        .map(|(edge_index, edge)| {
            let source = node_indices.get(edge.source.as_str()).copied().ok_or(
                DiagramRenderError::UnknownEdgeSource {
                    edge_index,
                    source: edge.source.clone(),
                },
            )?;
            let target = node_indices.get(edge.target.as_str()).copied().ok_or(
                DiagramRenderError::UnknownEdgeTarget {
                    edge_index,
                    target: edge.target.clone(),
                },
            )?;
            Ok((source, target))
        })
        .collect()
}
