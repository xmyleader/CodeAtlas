use std::{error::Error, fmt};

/// An error produced while validating or rendering an SVG diagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagramRenderError {
    /// The diagram contains more nodes than the renderer permits.
    TooManyNodes { actual: usize, maximum: usize },
    /// The diagram contains more edges than the renderer permits.
    TooManyEdges { actual: usize, maximum: usize },
    /// The UTF-8 title is too large.
    TitleTooLong { actual: usize, maximum: usize },
    /// A node label is too large in UTF-8 bytes.
    NodeLabelTooLong {
        node_index: usize,
        actual: usize,
        maximum: usize,
    },
    /// An edge label is too large in UTF-8 bytes.
    EdgeLabelTooLong {
        edge_index: usize,
        actual: usize,
        maximum: usize,
    },
    /// Two nodes use the same model ID.
    DuplicateNodeId {
        first_node_index: usize,
        duplicate_node_index: usize,
    },
    /// An edge refers to a source node that is not present.
    UnknownEdgeSource { edge_index: usize, source: String },
    /// An edge refers to a target node that is not present.
    UnknownEdgeTarget { edge_index: usize, target: String },
    /// The computed SVG width exceeds the canvas limit.
    WidthLimitExceeded { actual: usize, maximum: usize },
    /// The computed SVG height exceeds the canvas limit.
    HeightLimitExceeded { actual: usize, maximum: usize },
    /// The serialized SVG exceeds the output byte-size limit.
    SvgSizeLimitExceeded { actual: usize, maximum: usize },
}

impl fmt::Display for DiagramRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyNodes { actual, maximum } => {
                write!(
                    formatter,
                    "diagram has {actual} nodes; maximum is {maximum}"
                )
            }
            Self::TooManyEdges { actual, maximum } => {
                write!(
                    formatter,
                    "diagram has {actual} edges; maximum is {maximum}"
                )
            }
            Self::TitleTooLong { actual, maximum } => write!(
                formatter,
                "diagram title is {actual} bytes; maximum is {maximum}"
            ),
            Self::NodeLabelTooLong {
                node_index,
                actual,
                maximum,
            } => write!(
                formatter,
                "node {node_index} label is {actual} bytes; maximum is {maximum}"
            ),
            Self::EdgeLabelTooLong {
                edge_index,
                actual,
                maximum,
            } => write!(
                formatter,
                "edge {edge_index} label is {actual} bytes; maximum is {maximum}"
            ),
            Self::DuplicateNodeId {
                first_node_index,
                duplicate_node_index,
            } => write!(
                formatter,
                "node {duplicate_node_index} duplicates the ID of node {first_node_index}"
            ),
            Self::UnknownEdgeSource { edge_index, source } => {
                write!(formatter, "edge {edge_index} has unknown source {source:?}")
            }
            Self::UnknownEdgeTarget { edge_index, target } => {
                write!(formatter, "edge {edge_index} has unknown target {target:?}")
            }
            Self::WidthLimitExceeded { actual, maximum } => {
                write!(formatter, "SVG width is {actual}; maximum is {maximum}")
            }
            Self::HeightLimitExceeded { actual, maximum } => {
                write!(formatter, "SVG height is {actual}; maximum is {maximum}")
            }
            Self::SvgSizeLimitExceeded { actual, maximum } => {
                write!(formatter, "SVG is {actual} bytes; maximum is {maximum}")
            }
        }
    }
}

impl Error for DiagramRenderError {}
