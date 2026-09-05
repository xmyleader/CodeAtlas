#![allow(clippy::expect_used, clippy::too_many_lines)]

use codeatlas_core::{
    ClaimId, Diagram, DiagramArtifact, DiagramEdge, DiagramId, DiagramKind, DiagramNode, EvidenceId,
};
use codeatlas_diagram::{DiagramRenderError, render_svg};

fn claim_id(value: &str) -> ClaimId {
    ClaimId::from_stable_parts(&[value])
}

fn evidence_id(value: &str) -> EvidenceId {
    EvidenceId::from_stable_parts(&[value])
}

fn node(id: &str, label: &str) -> DiagramNode {
    DiagramNode {
        id: id.to_owned(),
        label: label.to_owned(),
        claim_ids: vec![claim_id(id)],
        evidence_ids: vec![evidence_id(id)],
    }
}

fn edge(source: &str, target: &str, label: &str) -> DiagramEdge {
    DiagramEdge {
        source: source.to_owned(),
        target: target.to_owned(),
        label: label.to_owned(),
        claim_ids: vec![ClaimId::from_stable_parts(&["edge", source, target, label])],
        evidence_ids: vec![EvidenceId::from_stable_parts(&[
            "edge", source, target, label,
        ])],
    }
}

fn diagram(
    kind: DiagramKind,
    title: &str,
    nodes: Vec<DiagramNode>,
    edges: Vec<DiagramEdge>,
) -> Diagram {
    Diagram {
        kind,
        title: title.to_owned(),
        nodes,
        edges,
        artifact: None,
    }
}

fn render_text(diagram: &Diagram) -> String {
    String::from_utf8(render_svg(diagram).expect("diagram should render as UTF-8 SVG"))
        .expect("SVG bytes should be UTF-8")
}

fn node_group<'a>(svg: &'a str, title: &str) -> &'a str {
    let title_marker = format!("    <title>{title}</title>");
    let title_offset = svg
        .find(&title_marker)
        .expect("node title should occur in SVG");
    let group_start = svg[..title_offset]
        .rfind("  <g id=\"node-")
        .expect("node group should precede its title");
    let group_end = svg[title_offset..]
        .find("  </g>")
        .map(|offset| title_offset + offset + "  </g>".len())
        .expect("node group should close");
    &svg[group_start..group_end]
}

fn rect_coordinate(group: &str, attribute: &str) -> i32 {
    let rect_start = group.find("    <rect ").expect("node rect should exist");
    let rect = &group[rect_start..];
    let marker = format!("{attribute}=\"");
    let value_start = rect.find(&marker).expect("rect attribute should exist") + marker.len();
    let value_end = rect[value_start..]
        .find('"')
        .map(|offset| value_start + offset)
        .expect("rect attribute should terminate");
    rect[value_start..value_end]
        .parse()
        .expect("rect coordinate should be an integer")
}

#[test]
fn architecture_is_left_to_right_and_marks_cycle_edges() {
    let diagram = diagram(
        DiagramKind::Architecture,
        "Runtime architecture",
        vec![node("c", "Worker"), node("a", "API"), node("b", "Queue")],
        vec![
            edge("a", "b", "publishes"),
            edge("b", "c", "dispatches"),
            edge("c", "b", "retries"),
        ],
    );

    let svg = render_text(&diagram);
    let api_x = rect_coordinate(node_group(&svg, "API"), "x");
    let queue_x = rect_coordinate(node_group(&svg, "Queue"), "x");
    let worker_x = rect_coordinate(node_group(&svg, "Worker"), "x");

    assert!(api_x < queue_x);
    assert_eq!(queue_x, worker_x);
    assert_eq!(svg.matches("stroke-dasharray=\"8 7\"").count(), 2);
    assert!(svg.contains("data-kind=\"architecture\""));
    assert!(svg.contains("CodeAtlas Architecture diagram. Title: Runtime architecture"));
}

#[test]
fn flow_is_top_down_and_keeps_input_order_within_a_level() {
    let diagram = diagram(
        DiagramKind::Flow,
        "Request flow",
        vec![
            node("z", "Receive"),
            node("a", "Validate"),
            node("m", "Persist"),
        ],
        vec![edge("z", "a", "valid path"), edge("z", "m", "audit path")],
    );

    let svg = render_text(&diagram);
    let receive = node_group(&svg, "Receive");
    let validate = node_group(&svg, "Validate");
    let persist = node_group(&svg, "Persist");

    assert!(rect_coordinate(receive, "y") < rect_coordinate(validate, "y"));
    assert_eq!(
        rect_coordinate(validate, "y"),
        rect_coordinate(persist, "y")
    );
    assert!(rect_coordinate(validate, "x") < rect_coordinate(persist, "x"));
    assert!(receive.contains("id=\"node-0\""));
    assert!(validate.contains("id=\"node-1\""));
    assert!(persist.contains("id=\"node-2\""));
}

#[test]
fn relationship_centers_highest_degree_and_separates_directions() {
    let diagram = diagram(
        DiagramKind::Relationship,
        "Ownership",
        vec![
            node("e-rest", "Utility"),
            node("c-out-1", "Report"),
            node("a-center", "Account"),
            node("b-in", "Operator"),
            node("d-out-2", "Audit"),
        ],
        vec![
            edge("b-in", "a-center", "owns"),
            edge("a-center", "c-out-1", "produces"),
            edge("a-center", "d-out-2", "records"),
            edge("e-rest", "b-in", "assists"),
        ],
    );

    let svg = render_text(&diagram);
    let center = node_group(&svg, "Account");
    let incoming = node_group(&svg, "Operator");
    let outgoing = node_group(&svg, "Report");
    let remaining = node_group(&svg, "Utility");
    let center_x = rect_coordinate(center, "x");

    assert!(center.contains("class=\"node focus-node\""));
    assert!(rect_coordinate(incoming, "x") < center_x);
    assert!(rect_coordinate(outgoing, "x") > center_x);
    assert!(rect_coordinate(remaining, "y") > rect_coordinate(center, "y"));
}

#[test]
fn rendering_is_byte_deterministic_and_architecture_is_canonical() {
    let original = diagram(
        DiagramKind::Architecture,
        "Deterministic",
        vec![
            node("gamma", "Gamma"),
            node("alpha", "Alpha"),
            node("beta", "Beta"),
        ],
        vec![edge("alpha", "beta", "one"), edge("beta", "gamma", "two")],
    );
    let first = render_svg(&original).expect("first render should succeed");
    let second = render_svg(&original).expect("second render should succeed");
    assert_eq!(first, second);

    let mut reordered = original.clone();
    reordered.nodes.reverse();
    reordered.edges.reverse();
    assert_eq!(
        first,
        render_svg(&reordered).expect("canonical render should succeed")
    );
}

#[test]
fn model_text_is_escaped_controls_are_filtered_and_artifact_is_ignored() {
    let hostile_id = "node\"><script id='model-id'>";
    let mut diagram = diagram(
        DiagramKind::Architecture,
        "Title </title><script>alert(\"x\")</script>\0\u{1}&",
        vec![
            node(
                hostile_id,
                "<foreignObject href=\"https://evil.test\"><img onload='x'>\u{b}",
            ),
            node("safe", "Safe & sound"),
        ],
        vec![edge(hostile_id, "safe", "\"><script>& payload")],
    );
    diagram.artifact = Some(DiagramArtifact {
        id: DiagramId::from_stable_parts(&["hostile-artifact"]),
        path: "https://evil.test/payload.svg".to_owned(),
        media_type: r#"image/svg+xml"><script>"#.to_owned(),
        byte_size: 10,
    });

    let svg = render_text(&diagram);

    assert!(
        svg.contains("Title &lt;/title&gt;&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;&amp;")
    );
    assert!(svg.contains("&lt;foreignObject href=&quot;https://evil.test&quot;&gt;"));
    assert!(!svg.contains("<script"));
    assert!(!svg.contains("<foreignObject"));
    assert!(!svg.contains(" href=\""));
    assert!(!svg.contains("xlink:href"));
    assert!(!svg.contains('\0'));
    assert!(!svg.contains('\u{1}'));
    assert!(!svg.contains('\u{b}'));
    assert!(!svg.contains(hostile_id));
    assert!(!svg.contains("https://evil.test/payload.svg"));
}

#[test]
fn labels_wrap_english_and_chinese_without_truncating_titles() {
    let english = "a deliberately long English node label that wraps at stable word boundaries";
    let chinese = "这是一个用于验证中文标签稳定有界换行行为的节点名称";
    let diagram = diagram(
        DiagramKind::Relationship,
        "Wrapping",
        vec![node("a", english), node("b", chinese)],
        vec![edge(
            "a",
            "b",
            "long edge label with English words 中文关系标签",
        )],
    );

    let svg = render_text(&diagram);
    let english_group = node_group(&svg, english);
    let chinese_group = node_group(&svg, chinese);

    assert!(english_group.matches("<tspan ").count() > 1);
    assert!(chinese_group.matches("<tspan ").count() > 1);
    assert!(english_group.contains(&format!("<title>{english}</title>")));
    assert!(chinese_group.contains(&format!("<title>{chinese}</title>")));
}

#[test]
fn count_and_text_limits_have_independent_errors() {
    let too_many_nodes = diagram(
        DiagramKind::Relationship,
        "nodes",
        (0..33)
            .map(|index| node(&format!("n-{index}"), "node"))
            .collect(),
        Vec::new(),
    );
    assert!(matches!(
        render_svg(&too_many_nodes),
        Err(DiagramRenderError::TooManyNodes {
            actual: 33,
            maximum: 32
        })
    ));

    let too_many_edges = diagram(
        DiagramKind::Architecture,
        "edges",
        vec![node("a", "A"), node("b", "B")],
        (0..65).map(|_| edge("a", "b", "edge")).collect(),
    );
    assert!(matches!(
        render_svg(&too_many_edges),
        Err(DiagramRenderError::TooManyEdges {
            actual: 65,
            maximum: 64
        })
    ));

    let long_title = diagram(
        DiagramKind::Relationship,
        &"t".repeat(257),
        vec![node("a", "A")],
        Vec::new(),
    );
    assert!(matches!(
        render_svg(&long_title),
        Err(DiagramRenderError::TitleTooLong {
            actual: 257,
            maximum: 256
        })
    ));

    let long_node = diagram(
        DiagramKind::Relationship,
        "node label",
        vec![node("a", &"n".repeat(129))],
        Vec::new(),
    );
    assert!(matches!(
        render_svg(&long_node),
        Err(DiagramRenderError::NodeLabelTooLong {
            node_index: 0,
            actual: 129,
            maximum: 128
        })
    ));

    let long_edge = diagram(
        DiagramKind::Architecture,
        "edge label",
        vec![node("a", "A"), node("b", "B")],
        vec![edge("a", "b", &"e".repeat(129))],
    );
    assert!(matches!(
        render_svg(&long_edge),
        Err(DiagramRenderError::EdgeLabelTooLong {
            edge_index: 0,
            actual: 129,
            maximum: 128
        })
    ));
}

#[test]
fn canvas_and_svg_size_limits_are_reported() {
    let wide_flow = diagram(
        DiagramKind::Flow,
        "wide",
        (0..18)
            .map(|index| node(&format!("node-{index}"), "node"))
            .collect(),
        Vec::new(),
    );
    assert!(matches!(
        render_svg(&wide_flow),
        Err(DiagramRenderError::WidthLimitExceeded { maximum: 4096, .. })
    ));

    let chain_nodes: Vec<_> = (0..32)
        .map(|index| node(&format!("node-{index}"), &"x".repeat(128)))
        .collect();
    let chain_edges = (0..31)
        .map(|index| {
            edge(
                &format!("node-{index}"),
                &format!("node-{}", index + 1),
                "next",
            )
        })
        .collect();
    let tall_flow = diagram(DiagramKind::Flow, "tall", chain_nodes, chain_edges);
    assert!(matches!(
        render_svg(&tall_flow),
        Err(DiagramRenderError::HeightLimitExceeded { maximum: 4096, .. })
    ));

    let repeated_claim = claim_id("large-output");
    let mut large_output = diagram(
        DiagramKind::Relationship,
        "large output",
        vec![node("a", "A"), node("b", "B")],
        vec![edge("a", "b", "edge")],
    );
    for node in &mut large_output.nodes {
        node.claim_ids = vec![repeated_claim; 12_000];
    }
    large_output.edges[0].claim_ids = vec![repeated_claim; 12_000];
    assert!(matches!(
        render_svg(&large_output),
        Err(DiagramRenderError::SvgSizeLimitExceeded {
            maximum: 1_048_576,
            ..
        })
    ));
}

#[test]
fn exact_text_limits_are_accepted() {
    let diagram = diagram(
        DiagramKind::Relationship,
        &"t".repeat(256),
        vec![node("a", &"n".repeat(128)), node("b", "B")],
        vec![edge("a", "b", &"e".repeat(128))],
    );

    assert!(render_svg(&diagram).is_ok());
}

#[test]
fn groups_expose_bindings_titles_and_safe_sequence_ids() {
    let first_claim = claim_id("claim-1");
    let second_claim = claim_id("claim-2");
    let first_evidence = evidence_id("evidence-1");
    let second_evidence = evidence_id("evidence-2");
    let mut first_node = node("a", "Alpha");
    first_node.claim_ids = vec![first_claim, second_claim];
    first_node.evidence_ids = vec![first_evidence, second_evidence];
    let mut bound_edge = edge("a", "b", "calls");
    bound_edge.claim_ids = vec![second_claim];
    bound_edge.evidence_ids = vec![second_evidence];
    let diagram = diagram(
        DiagramKind::Architecture,
        "Bindings",
        vec![first_node, node("b", "Beta")],
        vec![bound_edge],
    );

    let svg = render_text(&diagram);
    let alpha = node_group(&svg, "Alpha");
    let expected_claims = format!("data-claim-ids=\"{first_claim} {second_claim}\"");
    let expected_evidence = format!("data-evidence-ids=\"{first_evidence} {second_evidence}\"");

    assert!(alpha.contains("id=\"node-0\""));
    assert!(alpha.contains(&expected_claims));
    assert!(alpha.contains(&expected_evidence));
    assert!(alpha.contains("<title>Alpha</title>"));
    assert!(svg.contains("id=\"edge-0\""));
    assert!(svg.contains(&format!("data-claim-ids=\"{second_claim}\"")));
    assert!(svg.contains(&format!("data-evidence-ids=\"{second_evidence}\"")));
    assert!(svg.contains("<title>Alpha -&gt; Beta: calls</title>"));
}

#[test]
fn malformed_graph_references_are_errors() {
    let duplicate = diagram(
        DiagramKind::Architecture,
        "duplicate",
        vec![node("same", "A"), node("same", "B")],
        Vec::new(),
    );
    assert!(matches!(
        render_svg(&duplicate),
        Err(DiagramRenderError::DuplicateNodeId {
            first_node_index: 0,
            duplicate_node_index: 1
        })
    ));

    let unknown = diagram(
        DiagramKind::Flow,
        "unknown",
        vec![node("a", "A")],
        vec![edge("a", "missing", "next")],
    );
    assert!(matches!(
        render_svg(&unknown),
        Err(DiagramRenderError::UnknownEdgeTarget { edge_index: 0, .. })
    ));
}

#[test]
fn svg_has_a_self_contained_basic_structure() {
    let diagram = diagram(
        DiagramKind::Architecture,
        "Structure",
        vec![node("a", "A"), node("b", "B")],
        vec![edge("a", "b", "calls")],
    );
    let svg = render_text(&diagram);

    assert!(svg.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg "));
    assert!(svg.contains("xmlns=\"http://www.w3.org/2000/svg\""));
    assert!(svg.contains("viewBox=\"0 0 "));
    assert!(svg.contains("<defs>"));
    assert!(svg.contains("id=\"arrowhead-0\""));
    assert!(svg.contains("marker-end=\"url(#arrowhead-0)\""));
    assert!(svg.contains("rx=\"12\""));
    assert!(svg.contains("<path "));
    assert!(svg.contains("<text "));
    assert!(svg.ends_with("</svg>\n"));
    assert_eq!(svg.matches("class=\"node\"").count(), 2);
    assert_eq!(svg.matches("class=\"edge\"").count(), 1);
}
