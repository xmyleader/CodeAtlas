use codeatlas_core::{Diagram, DiagramKind};

pub(crate) const OUTER_MARGIN: i32 = 152;
pub(crate) const HEADER_TITLE_Y: i32 = 38;
pub(crate) const HEADER_KIND_Y: i32 = 64;
pub(crate) const HEADER_HEIGHT: i32 = 86;
pub(crate) const NODE_LINE_HEIGHT: i32 = 18;
pub(crate) const EDGE_LINE_HEIGHT: i32 = 16;

const MIN_CANVAS_WIDTH: i32 = 480;
const CONTENT_TOP_GAP: i32 = 32;
const NODE_WIDTH: i32 = 216;
const NODE_MIN_HEIGHT: i32 = 62;
const NODE_VERTICAL_PADDING: i32 = 28;
const NODE_LABEL_UNITS: usize = 22;
const EDGE_LABEL_UNITS: usize = 22;
const ARCHITECTURE_LAYER_GAP: i32 = 96;
const FLOW_LAYER_GAP: i32 = 58;
const NODE_GAP: i32 = 24;
const RELATIONSHIP_GAP: i32 = 116;
const RELATIONSHIP_REST_COLUMNS: usize = 4;
const TITLE_UNIT_WIDTH: i32 = 12;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Rect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

impl Rect {
    pub(crate) const fn right(self) -> i32 {
        self.x + self.width
    }

    pub(crate) const fn bottom(self) -> i32 {
        self.y + self.height
    }

    pub(crate) const fn center_x(self) -> i32 {
        self.x + self.width / 2
    }

    pub(crate) const fn center_y(self) -> i32 {
        self.y + self.height / 2
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Point {
    pub(crate) x: i32,
    pub(crate) y: i32,
}

#[derive(Debug)]
pub(crate) struct BezierPath {
    pub(crate) start: Point,
    pub(crate) control_1: Point,
    pub(crate) control_2: Point,
    pub(crate) end: Point,
}

#[derive(Debug)]
pub(crate) struct NodeLayout {
    pub(crate) diagram_index: usize,
    pub(crate) rect: Rect,
    pub(crate) lines: Vec<String>,
    pub(crate) focused: bool,
}

#[derive(Debug)]
pub(crate) struct EdgeLayout {
    pub(crate) diagram_index: usize,
    pub(crate) source_index: usize,
    pub(crate) target_index: usize,
    pub(crate) path: BezierPath,
    pub(crate) label_position: Point,
    pub(crate) label_lines: Vec<String>,
    pub(crate) dashed: bool,
}

#[derive(Debug)]
pub(crate) struct Layout {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) nodes: Vec<NodeLayout>,
    pub(crate) edges: Vec<EdgeLayout>,
}

struct RelationshipGroups {
    center: usize,
    incoming: Vec<usize>,
    outgoing: Vec<usize>,
    remaining: Vec<usize>,
}

pub(crate) fn build(diagram: &Diagram, endpoints: &[(usize, usize)]) -> Layout {
    let canonical_nodes = canonical_node_order(diagram);
    let node_order = match diagram.kind {
        DiagramKind::Flow => (0..diagram.nodes.len()).collect(),
        DiagramKind::Architecture | DiagramKind::Relationship => canonical_nodes.clone(),
    };
    let edge_order = edge_order(diagram, endpoints, &canonical_nodes);
    let lines: Vec<_> = diagram
        .nodes
        .iter()
        .map(|node| wrap_text(&node.label, NODE_LABEL_UNITS))
        .collect();
    let heights: Vec<_> = lines.iter().map(|value| node_height(value.len())).collect();

    let (rects, width, height, focus, cycle_edges) = match diagram.kind {
        DiagramKind::Architecture => {
            let (layers, cycle_edges) = graph_layers(diagram.nodes.len(), endpoints, &node_order);
            let (rects, width, height) = architecture_positions(diagram, &layers, &heights);
            (rects, width, height, None, cycle_edges)
        }
        DiagramKind::Flow => {
            let (layers, cycle_edges) = graph_layers(diagram.nodes.len(), endpoints, &node_order);
            let (rects, width, height) = flow_positions(diagram, &layers, &heights, &node_order);
            (rects, width, height, None, cycle_edges)
        }
        DiagramKind::Relationship => {
            let (_, cycle_edges) = graph_layers(diagram.nodes.len(), endpoints, &canonical_nodes);
            let (rects, width, height, focus) =
                relationship_positions(diagram, endpoints, &heights, &canonical_nodes);
            (rects, width, height, focus, cycle_edges)
        }
    };

    let nodes = node_order
        .into_iter()
        .map(|diagram_index| NodeLayout {
            diagram_index,
            rect: rects[diagram_index],
            lines: lines[diagram_index].clone(),
            focused: focus == Some(diagram_index),
        })
        .collect();
    let edges = edge_order
        .into_iter()
        .map(|diagram_index| {
            let (source_index, target_index) = endpoints[diagram_index];
            let (path, label_position) = route_edge(
                diagram.kind,
                rects[source_index],
                rects[target_index],
                source_index == target_index,
                cycle_edges[diagram_index],
            );
            EdgeLayout {
                diagram_index,
                source_index,
                target_index,
                path,
                label_position,
                label_lines: wrap_text(&diagram.edges[diagram_index].label, EDGE_LABEL_UNITS),
                dashed: cycle_edges[diagram_index],
            }
        })
        .collect();

    Layout {
        width,
        height,
        nodes,
        edges,
    }
}

fn canonical_node_order(diagram: &Diagram) -> Vec<usize> {
    let mut order: Vec<_> = (0..diagram.nodes.len()).collect();
    order.sort_by(|left, right| diagram.nodes[*left].id.cmp(&diagram.nodes[*right].id));
    order
}

fn edge_order(
    diagram: &Diagram,
    endpoints: &[(usize, usize)],
    canonical_nodes: &[usize],
) -> Vec<usize> {
    let mut order: Vec<_> = (0..diagram.edges.len()).collect();
    if diagram.kind == DiagramKind::Flow {
        return order;
    }

    let mut canonical_rank = vec![0; diagram.nodes.len()];
    for (rank, node_index) in canonical_nodes.iter().copied().enumerate() {
        canonical_rank[node_index] = rank;
    }
    order.sort_by(|left_index, right_index| {
        let left = &diagram.edges[*left_index];
        let right = &diagram.edges[*right_index];
        let (left_source, left_target) = endpoints[*left_index];
        let (right_source, right_target) = endpoints[*right_index];

        canonical_rank[left_source]
            .cmp(&canonical_rank[right_source])
            .then_with(|| canonical_rank[left_target].cmp(&canonical_rank[right_target]))
            .then_with(|| left.label.cmp(&right.label))
            .then_with(|| left.claim_ids.cmp(&right.claim_ids))
            .then_with(|| left.evidence_ids.cmp(&right.evidence_ids))
    });
    order
}

fn graph_layers(
    node_count: usize,
    endpoints: &[(usize, usize)],
    semantic_order: &[usize],
) -> (Vec<usize>, Vec<bool>) {
    if node_count == 0 {
        return (Vec::new(), vec![false; endpoints.len()]);
    }

    let mut reachable = vec![vec![false; node_count]; node_count];
    for (node_index, row) in reachable.iter_mut().enumerate() {
        row[node_index] = true;
    }
    for &(source, target) in endpoints {
        reachable[source][target] = true;
    }
    for via in 0..node_count {
        let via_reachable = reachable[via].clone();
        for row in &mut reachable {
            if row[via] {
                for (cell, via_cell) in row.iter_mut().zip(&via_reachable) {
                    *cell |= *via_cell;
                }
            }
        }
    }

    let mut component_of = vec![usize::MAX; node_count];
    let mut component_count = 0;
    for &seed in semantic_order {
        if component_of[seed] != usize::MAX {
            continue;
        }
        for &candidate in semantic_order {
            if component_of[candidate] == usize::MAX
                && reachable[seed][candidate]
                && reachable[candidate][seed]
            {
                component_of[candidate] = component_count;
            }
        }
        component_count += 1;
    }

    let mut component_edges = vec![vec![false; component_count]; component_count];
    for &(source, target) in endpoints {
        let source_component = component_of[source];
        let target_component = component_of[target];
        if source_component != target_component {
            component_edges[source_component][target_component] = true;
        }
    }
    let mut indegrees = vec![0_usize; component_count];
    for row in &component_edges {
        for (target_component, has_edge) in row.iter().copied().enumerate() {
            if has_edge {
                indegrees[target_component] += 1;
            }
        }
    }

    let mut component_layers = vec![0_usize; component_count];
    let mut processed = vec![false; component_count];
    for _ in 0..component_count {
        let component = (0..component_count)
            .find(|candidate| !processed[*candidate] && indegrees[*candidate] == 0)
            .expect("the SCC condensation graph is acyclic");
        processed[component] = true;
        for (target, has_edge) in component_edges[component].iter().copied().enumerate() {
            if has_edge {
                component_layers[target] =
                    component_layers[target].max(component_layers[component] + 1);
                indegrees[target] -= 1;
            }
        }
    }

    let layers: Vec<_> = component_of
        .iter()
        .map(|component| component_layers[*component])
        .collect();
    let cycle_edges = endpoints
        .iter()
        .map(|&(source, target)| {
            component_of[source] == component_of[target] || layers[target] <= layers[source]
        })
        .collect();
    (layers, cycle_edges)
}

fn architecture_positions(
    diagram: &Diagram,
    layers: &[usize],
    heights: &[i32],
) -> (Vec<Rect>, i32, i32) {
    if diagram.nodes.is_empty() {
        return empty_positions(diagram);
    }

    let layer_count = layers.iter().copied().max().unwrap_or(0) + 1;
    let mut layer_nodes = vec![Vec::new(); layer_count];
    for node_index in canonical_node_order(diagram) {
        layer_nodes[layers[node_index]].push(node_index);
    }
    let layer_heights: Vec<_> = layer_nodes
        .iter()
        .map(|nodes| stack_height(nodes, heights))
        .collect();
    let body_height = layer_heights.iter().copied().max().unwrap_or(0);
    let body_width = repeated_extent(layer_count, NODE_WIDTH, ARCHITECTURE_LAYER_GAP);
    let width = required_width(diagram, body_width);
    let body_start_x = (width - body_width) / 2;
    let body_start_y = HEADER_HEIGHT + CONTENT_TOP_GAP;
    let mut rects = vec![Rect::default(); diagram.nodes.len()];

    for (layer, nodes) in layer_nodes.iter().enumerate() {
        let mut y = body_start_y + (body_height - layer_heights[layer]) / 2;
        let x = body_start_x + to_i32(layer) * (NODE_WIDTH + ARCHITECTURE_LAYER_GAP);
        for &node_index in nodes {
            rects[node_index] = Rect {
                x,
                y,
                width: NODE_WIDTH,
                height: heights[node_index],
            };
            y += heights[node_index] + NODE_GAP;
        }
    }

    let height = body_start_y + body_height + OUTER_MARGIN;
    (rects, width, height)
}

fn flow_positions(
    diagram: &Diagram,
    layers: &[usize],
    heights: &[i32],
    semantic_order: &[usize],
) -> (Vec<Rect>, i32, i32) {
    if diagram.nodes.is_empty() {
        return empty_positions(diagram);
    }

    let layer_count = layers.iter().copied().max().unwrap_or(0) + 1;
    let mut layer_nodes = vec![Vec::new(); layer_count];
    for &node_index in semantic_order {
        layer_nodes[layers[node_index]].push(node_index);
    }
    let row_widths: Vec<_> = layer_nodes
        .iter()
        .map(|nodes| repeated_extent(nodes.len(), NODE_WIDTH, NODE_GAP))
        .collect();
    let row_heights: Vec<_> = layer_nodes
        .iter()
        .map(|nodes| {
            nodes
                .iter()
                .map(|node_index| heights[*node_index])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let body_width = row_widths.iter().copied().max().unwrap_or(0);
    let width = required_width(diagram, body_width);
    let body_start_y = HEADER_HEIGHT + CONTENT_TOP_GAP;
    let mut y = body_start_y;
    let mut rects = vec![Rect::default(); diagram.nodes.len()];

    for (layer, nodes) in layer_nodes.iter().enumerate() {
        let mut x = (width - row_widths[layer]) / 2;
        for &node_index in nodes {
            rects[node_index] = Rect {
                x,
                y: y + (row_heights[layer] - heights[node_index]) / 2,
                width: NODE_WIDTH,
                height: heights[node_index],
            };
            x += NODE_WIDTH + NODE_GAP;
        }
        y += row_heights[layer] + FLOW_LAYER_GAP;
    }
    y -= FLOW_LAYER_GAP;

    (rects, width, y + OUTER_MARGIN)
}

fn relationship_positions(
    diagram: &Diagram,
    endpoints: &[(usize, usize)],
    heights: &[i32],
    canonical_nodes: &[usize],
) -> (Vec<Rect>, i32, i32, Option<usize>) {
    if diagram.nodes.is_empty() {
        let (rects, width, height) = empty_positions(diagram);
        return (rects, width, height, None);
    }

    let groups = relationship_groups(diagram, endpoints, canonical_nodes);
    let center = groups.center;
    let incoming = groups.incoming;
    let outgoing = groups.outgoing;
    let remaining = groups.remaining;

    let incoming_height = stack_height(&incoming, heights);
    let outgoing_height = stack_height(&outgoing, heights);
    let main_height = incoming_height.max(outgoing_height).max(heights[center]);
    let main_width = NODE_WIDTH * 3 + RELATIONSHIP_GAP * 2;
    let rest_columns = remaining.len().min(RELATIONSHIP_REST_COLUMNS);
    let rest_width = repeated_extent(rest_columns, NODE_WIDTH, NODE_GAP);
    let body_width = main_width.max(rest_width);
    let width = required_width(diagram, body_width);
    let center_x = (width - NODE_WIDTH) / 2;
    let main_start_y = HEADER_HEIGHT + CONTENT_TOP_GAP;
    let mut rects = vec![Rect::default(); diagram.nodes.len()];

    place_stack(
        &mut rects,
        &incoming,
        heights,
        center_x - NODE_WIDTH - RELATIONSHIP_GAP,
        main_start_y + (main_height - incoming_height) / 2,
    );
    place_stack(
        &mut rects,
        &outgoing,
        heights,
        center_x + NODE_WIDTH + RELATIONSHIP_GAP,
        main_start_y + (main_height - outgoing_height) / 2,
    );
    rects[center] = Rect {
        x: center_x,
        y: main_start_y + (main_height - heights[center]) / 2,
        width: NODE_WIDTH,
        height: heights[center],
    };

    let mut body_bottom = main_start_y + main_height;
    if !remaining.is_empty() {
        let rest_start_y = body_bottom + FLOW_LAYER_GAP;
        let mut row_y = rest_start_y;
        for row in remaining.chunks(RELATIONSHIP_REST_COLUMNS) {
            let row_width = repeated_extent(row.len(), NODE_WIDTH, NODE_GAP);
            let row_height = row
                .iter()
                .map(|node_index| heights[*node_index])
                .max()
                .unwrap_or(0);
            let mut x = (width - row_width) / 2;
            for &node_index in row {
                rects[node_index] = Rect {
                    x,
                    y: row_y + (row_height - heights[node_index]) / 2,
                    width: NODE_WIDTH,
                    height: heights[node_index],
                };
                x += NODE_WIDTH + NODE_GAP;
            }
            row_y += row_height + NODE_GAP;
        }
        body_bottom = row_y - NODE_GAP;
    }

    (rects, width, body_bottom + OUTER_MARGIN, Some(center))
}

fn relationship_groups(
    diagram: &Diagram,
    endpoints: &[(usize, usize)],
    canonical_nodes: &[usize],
) -> RelationshipGroups {
    let mut degrees = vec![0_usize; diagram.nodes.len()];
    for &(source, target) in endpoints {
        degrees[source] += 1;
        degrees[target] += 1;
    }
    let center = canonical_nodes
        .iter()
        .copied()
        .reduce(|best, candidate| {
            if degrees[candidate] > degrees[best] {
                candidate
            } else {
                best
            }
        })
        .expect("a non-empty diagram has a relationship center");

    let mut incoming_flags = vec![false; diagram.nodes.len()];
    let mut outgoing_flags = vec![false; diagram.nodes.len()];
    for &(source, target) in endpoints {
        if target == center && source != center {
            incoming_flags[source] = true;
        }
        if source == center && target != center {
            outgoing_flags[target] = true;
        }
    }

    let mut groups = RelationshipGroups {
        center,
        incoming: Vec::new(),
        outgoing: Vec::new(),
        remaining: Vec::new(),
    };
    for &node_index in canonical_nodes {
        if node_index == center {
            continue;
        }
        if incoming_flags[node_index] {
            groups.incoming.push(node_index);
        } else if outgoing_flags[node_index] {
            groups.outgoing.push(node_index);
        } else {
            groups.remaining.push(node_index);
        }
    }
    groups
}

fn place_stack(rects: &mut [Rect], nodes: &[usize], heights: &[i32], x: i32, mut y: i32) {
    for &node_index in nodes {
        rects[node_index] = Rect {
            x,
            y,
            width: NODE_WIDTH,
            height: heights[node_index],
        };
        y += heights[node_index] + NODE_GAP;
    }
}

fn route_edge(
    kind: DiagramKind,
    source: Rect,
    target: Rect,
    self_edge: bool,
    dashed: bool,
) -> (BezierPath, Point) {
    if self_edge {
        return self_loop(source);
    }
    match kind {
        DiagramKind::Architecture if !dashed && target.x > source.x => {
            horizontal_path(source, target)
        }
        DiagramKind::Architecture => same_column_path(source, target),
        DiagramKind::Flow if !dashed && target.y > source.y => vertical_path(source, target),
        DiagramKind::Flow => same_row_path(source, target),
        DiagramKind::Relationship => relationship_path(source, target),
    }
}

fn horizontal_path(source: Rect, target: Rect) -> (BezierPath, Point) {
    let start = Point {
        x: source.right(),
        y: source.center_y(),
    };
    let end = Point {
        x: target.x,
        y: target.center_y(),
    };
    let middle_x = (start.x + end.x) / 2;
    (
        BezierPath {
            start,
            control_1: Point {
                x: middle_x,
                y: start.y,
            },
            control_2: Point {
                x: middle_x,
                y: end.y,
            },
            end,
        },
        Point {
            x: middle_x,
            y: (start.y + end.y) / 2 - 10,
        },
    )
}

fn vertical_path(source: Rect, target: Rect) -> (BezierPath, Point) {
    let start = Point {
        x: source.center_x(),
        y: source.bottom(),
    };
    let end = Point {
        x: target.center_x(),
        y: target.y,
    };
    let middle_y = (start.y + end.y) / 2;
    (
        BezierPath {
            start,
            control_1: Point {
                x: start.x,
                y: middle_y,
            },
            control_2: Point {
                x: end.x,
                y: middle_y,
            },
            end,
        },
        Point {
            x: (start.x + end.x) / 2,
            y: middle_y - 10,
        },
    )
}

fn same_column_path(source: Rect, target: Rect) -> (BezierPath, Point) {
    let route_x = source.right().max(target.right()) + 48;
    let start = Point {
        x: source.right(),
        y: source.center_y(),
    };
    let end = Point {
        x: target.right(),
        y: target.center_y(),
    };
    (
        BezierPath {
            start,
            control_1: Point {
                x: route_x,
                y: start.y,
            },
            control_2: Point {
                x: route_x,
                y: end.y,
            },
            end,
        },
        Point {
            x: route_x,
            y: (start.y + end.y) / 2,
        },
    )
}

fn same_row_path(source: Rect, target: Rect) -> (BezierPath, Point) {
    let route_y = source.bottom().max(target.bottom()) + 48;
    let start = Point {
        x: source.center_x(),
        y: source.bottom(),
    };
    let end = Point {
        x: target.center_x(),
        y: target.bottom(),
    };
    (
        BezierPath {
            start,
            control_1: Point {
                x: start.x,
                y: route_y,
            },
            control_2: Point {
                x: end.x,
                y: route_y,
            },
            end,
        },
        Point {
            x: (start.x + end.x) / 2,
            y: route_y,
        },
    )
}

fn relationship_path(source: Rect, target: Rect) -> (BezierPath, Point) {
    let horizontal_distance = (target.center_x() - source.center_x()).abs();
    let vertical_distance = (target.center_y() - source.center_y()).abs();
    if horizontal_distance >= vertical_distance {
        if target.center_x() >= source.center_x() {
            horizontal_path(source, target)
        } else {
            let (mut path, label) = horizontal_path(target, source);
            std::mem::swap(&mut path.start, &mut path.end);
            std::mem::swap(&mut path.control_1, &mut path.control_2);
            (path, label)
        }
    } else if target.center_y() >= source.center_y() {
        vertical_path(source, target)
    } else {
        let (mut path, label) = vertical_path(target, source);
        std::mem::swap(&mut path.start, &mut path.end);
        std::mem::swap(&mut path.control_1, &mut path.control_2);
        (path, label)
    }
}

fn self_loop(node: Rect) -> (BezierPath, Point) {
    let start = Point {
        x: node.right(),
        y: node.center_y() - 12,
    };
    let end = Point {
        x: node.right(),
        y: node.center_y() + 12,
    };
    let loop_x = node.right() + 52;
    (
        BezierPath {
            start,
            control_1: Point {
                x: loop_x,
                y: node.y - 24,
            },
            control_2: Point {
                x: loop_x,
                y: node.bottom() + 24,
            },
            end,
        },
        Point {
            x: loop_x,
            y: node.center_y(),
        },
    )
}

fn node_height(line_count: usize) -> i32 {
    (to_i32(line_count) * NODE_LINE_HEIGHT + NODE_VERTICAL_PADDING).max(NODE_MIN_HEIGHT)
}

fn stack_height(nodes: &[usize], heights: &[i32]) -> i32 {
    if nodes.is_empty() {
        return 0;
    }
    let node_height: i32 = nodes.iter().map(|node_index| heights[*node_index]).sum();
    node_height + to_i32(nodes.len() - 1) * NODE_GAP
}

fn repeated_extent(count: usize, item_extent: i32, gap: i32) -> i32 {
    if count == 0 {
        0
    } else {
        to_i32(count) * item_extent + to_i32(count - 1) * gap
    }
}

fn required_width(diagram: &Diagram, body_width: i32) -> i32 {
    let title_width = to_i32(display_units(&diagram.title)) * TITLE_UNIT_WIDTH;
    (body_width + OUTER_MARGIN * 2)
        .max(title_width + OUTER_MARGIN * 2)
        .max(MIN_CANVAS_WIDTH)
}

fn empty_positions(diagram: &Diagram) -> (Vec<Rect>, i32, i32) {
    (
        Vec::new(),
        required_width(diagram, 0),
        HEADER_HEIGHT + OUTER_MARGIN,
    )
}

pub(crate) fn display_units(value: &str) -> usize {
    value
        .chars()
        .filter(|character| is_xml_character(*character))
        .map(character_units)
        .sum()
}

fn wrap_text(value: &str, maximum_units: usize) -> Vec<String> {
    let cleaned: String = value
        .chars()
        .filter(|character| is_xml_character(*character))
        .collect();
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_units = 0;

    for word in cleaned.split_whitespace() {
        let word_units = display_units(word);
        if word_units <= maximum_units {
            if current.is_empty() {
                current.push_str(word);
                current_units = word_units;
            } else if current_units + 1 + word_units <= maximum_units {
                current.push(' ');
                current.push_str(word);
                current_units += 1 + word_units;
            } else {
                lines.push(std::mem::take(&mut current));
                current.push_str(word);
                current_units = word_units;
            }
            continue;
        }

        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_units = 0;
        }
        let chunks = split_word(word, maximum_units);
        let last_index = chunks.len().saturating_sub(1);
        for (chunk_index, (chunk, chunk_units)) in chunks.into_iter().enumerate() {
            if chunk_index == last_index {
                current = chunk;
                current_units = chunk_units;
            } else {
                lines.push(chunk);
            }
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn split_word(word: &str, maximum_units: usize) -> Vec<(String, usize)> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_units = 0;

    for character in word.chars() {
        let units = character_units(character);
        if !current.is_empty() && current_units + units > maximum_units {
            chunks.push((std::mem::take(&mut current), current_units));
            current_units = 0;
        }
        current.push(character);
        current_units += units;
    }
    if !current.is_empty() {
        chunks.push((current, current_units));
    }
    chunks
}

const fn character_units(character: char) -> usize {
    if character.is_ascii() { 1 } else { 2 }
}

pub(crate) const fn is_xml_character(character: char) -> bool {
    matches!(
        character,
        '\u{9}' | '\u{a}' | '\u{d}'
            | '\u{20}'..='\u{d7ff}'
            | '\u{e000}'..='\u{fffd}'
            | '\u{10000}'..='\u{10ffff}'
    )
}

fn to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}
