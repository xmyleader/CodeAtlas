use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    AnswerId, CallEdgeId, CallPathId, ClaimId, DiagramId, EvidenceId, ExplanationDepth, FileId,
    ModelUsage, RepositoryPath, SourceSpan, SymbolId, TargetResolution,
};

const LEGACY_DIAGRAM_REASON: &str = "legacy session did not include a diagram decision";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: EvidenceId,
    pub file_id: FileId,
    pub path: RepositoryPath,
    pub span: SourceSpan,
    pub symbol_id: Option<SymbolId>,
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    Fact,
    Inference,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub id: ClaimId,
    pub kind: ClaimKind,
    pub text: String,
    pub evidence_ids: Vec<EvidenceId>,
}

impl Claim {
    #[must_use]
    pub fn has_evidence(&self) -> bool {
        !self.evidence_ids.is_empty()
    }

    #[must_use]
    pub fn is_unsupported_fact(&self) -> bool {
        self.kind == ClaimKind::Fact && !self.has_evidence()
    }

    /// Checks the evidence requirement that applies to factual claims.
    ///
    /// # Errors
    ///
    /// Returns [`EvidenceValidationError::FactWithoutEvidence`] for a fact
    /// without any attached evidence IDs.
    pub fn validate_evidence(&self) -> Result<(), EvidenceValidationError> {
        if self.is_unsupported_fact() {
            Err(EvidenceValidationError::FactWithoutEvidence { claim_id: self.id })
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagramKind {
    Architecture,
    Flow,
    Relationship,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagramArtifact {
    pub id: DiagramId,
    pub path: String,
    pub media_type: String,
    pub byte_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum DiagramDecision {
    NotNeeded { reason: String },
    Needed { reason: String, diagram: Diagram },
}

impl Default for DiagramDecision {
    fn default() -> Self {
        Self::NotNeeded {
            reason: LEGACY_DIAGRAM_REASON.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagram {
    pub kind: DiagramKind,
    pub title: String,
    pub nodes: Vec<DiagramNode>,
    pub edges: Vec<DiagramEdge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<DiagramArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagramNode {
    pub id: String,
    pub label: String,
    pub claim_ids: Vec<ClaimId>,
    pub evidence_ids: Vec<EvidenceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagramEdge {
    pub source: String,
    pub target: String,
    pub label: String,
    pub claim_ids: Vec<ClaimId>,
    pub evidence_ids: Vec<EvidenceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallPathStep {
    pub target: TargetResolution<SymbolId>,
    pub call_edge_id: Option<CallEdgeId>,
    pub evidence_ids: Vec<EvidenceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallPath {
    pub id: CallPathId,
    pub label: Option<String>,
    pub steps: Vec<CallPathStep>,
    pub complete: bool,
}

/// A closed, non-executable follow-up capability offered by a grounded answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SuggestedAction {
    DeepenClaim { claim_id: ClaimId },
    ContinueCallPath { call_path_id: CallPathId },
    ExplainEvidence { evidence_id: EvidenceId },
    ShowSource { evidence_id: EvidenceId },
    ChangeDepth { depth: ExplanationDepth },
}

impl SuggestedAction {
    /// Returns a bounded runtime-owned label; models cannot supply action labels.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DeepenClaim { .. } => "Explain this claim in more depth",
            Self::ContinueCallPath { .. } => "Continue this call path",
            Self::ExplainEvidence { .. } => "Explain this evidence",
            Self::ShowSource { .. } => "Show source",
            Self::ChangeDepth { depth } => match depth {
                ExplanationDepth::Auto => "Use automatic depth",
                ExplanationDepth::Overview => "Switch to overview",
                ExplanationDepth::Architecture => "Explore the architecture",
                ExplanationDepth::Workflow => "Trace the workflow",
                ExplanationDepth::Code => "Explain the code",
                ExplanationDepth::Detail => "Go into full detail",
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentAnswer {
    pub id: AnswerId,
    pub text: String,
    pub claims: Vec<Claim>,
    pub evidence: Vec<Evidence>,
    pub call_paths: Vec<CallPath>,
    #[serde(default)]
    pub diagram: DiagramDecision,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_actions: Vec<SuggestedAction>,
    pub usage: Option<ModelUsage>,
}

impl AgentAnswer {
    /// Validates factual support and all evidence references in this answer.
    ///
    /// # Errors
    ///
    /// Returns [`EvidenceValidationError`] for duplicate answer-local IDs,
    /// unsupported facts, or references to evidence not carried by the answer.
    pub fn validate_evidence(&self) -> Result<(), EvidenceValidationError> {
        let mut known = HashSet::with_capacity(self.evidence.len());
        for evidence in &self.evidence {
            if !known.insert(evidence.id) {
                return Err(EvidenceValidationError::DuplicateEvidence {
                    evidence_id: evidence.id,
                });
            }
        }

        let mut known_claims = HashMap::with_capacity(self.claims.len());
        for claim in &self.claims {
            claim.validate_evidence()?;
            validate_references(claim.id, &claim.evidence_ids, &known)?;
            if known_claims.insert(claim.id, claim).is_some() {
                return Err(EvidenceValidationError::DuplicateClaim { claim_id: claim.id });
            }
        }

        let mut known_paths = HashSet::with_capacity(self.call_paths.len());
        for path in &self.call_paths {
            if !known_paths.insert(path.id) {
                return Err(EvidenceValidationError::DuplicateCallPath {
                    call_path_id: path.id,
                });
            }
            for step in &path.steps {
                for evidence_id in &step.evidence_ids {
                    if !known.contains(evidence_id) {
                        return Err(EvidenceValidationError::UnknownCallPathEvidence {
                            call_path_id: path.id,
                            evidence_id: *evidence_id,
                        });
                    }
                }
            }
        }

        validate_diagram_decision(&self.diagram, &known_claims, &known)?;
        self.validate_suggested_actions(&known_claims, &known_paths, &known)
    }

    fn validate_suggested_actions(
        &self,
        known_claims: &HashMap<ClaimId, &Claim>,
        known_paths: &HashSet<CallPathId>,
        known_evidence: &HashSet<EvidenceId>,
    ) -> Result<(), EvidenceValidationError> {
        if self.suggested_actions.len() > 4 {
            return Err(EvidenceValidationError::TooManySuggestedActions {
                actual: self.suggested_actions.len(),
            });
        }
        let mut unique = HashSet::with_capacity(self.suggested_actions.len());
        for action in &self.suggested_actions {
            if !unique.insert(*action) {
                return Err(EvidenceValidationError::DuplicateSuggestedAction);
            }
            match action {
                SuggestedAction::DeepenClaim { claim_id }
                    if !known_claims.contains_key(claim_id) =>
                {
                    return Err(EvidenceValidationError::UnknownSuggestedClaim {
                        claim_id: *claim_id,
                    });
                }
                SuggestedAction::ContinueCallPath { call_path_id }
                    if !known_paths.contains(call_path_id) =>
                {
                    return Err(EvidenceValidationError::UnknownSuggestedCallPath {
                        call_path_id: *call_path_id,
                    });
                }
                SuggestedAction::ExplainEvidence { evidence_id }
                | SuggestedAction::ShowSource { evidence_id }
                    if !known_evidence.contains(evidence_id) =>
                {
                    return Err(EvidenceValidationError::UnknownSuggestedEvidence {
                        evidence_id: *evidence_id,
                    });
                }
                SuggestedAction::DeepenClaim { .. }
                | SuggestedAction::ContinueCallPath { .. }
                | SuggestedAction::ExplainEvidence { .. }
                | SuggestedAction::ShowSource { .. }
                | SuggestedAction::ChangeDepth { .. } => {}
            }
        }
        Ok(())
    }
}

fn validate_references(
    claim_id: ClaimId,
    evidence_ids: &[EvidenceId],
    known: &HashSet<EvidenceId>,
) -> Result<(), EvidenceValidationError> {
    for evidence_id in evidence_ids {
        if !known.contains(evidence_id) {
            return Err(EvidenceValidationError::UnknownClaimEvidence {
                claim_id,
                evidence_id: *evidence_id,
            });
        }
    }
    Ok(())
}

fn validate_diagram_decision(
    decision: &DiagramDecision,
    known_claims: &HashMap<ClaimId, &Claim>,
    known_evidence: &HashSet<EvidenceId>,
) -> Result<(), EvidenceValidationError> {
    match decision {
        DiagramDecision::NotNeeded { reason } => validate_diagram_reason(reason),
        DiagramDecision::Needed { reason, diagram } => {
            validate_diagram_reason(reason)?;
            validate_diagram(diagram, known_claims, known_evidence)
        }
    }
}

fn validate_diagram_reason(reason: &str) -> Result<(), EvidenceValidationError> {
    if reason.trim().is_empty() {
        Err(EvidenceValidationError::EmptyDiagramDecisionReason)
    } else {
        Ok(())
    }
}

fn validate_diagram(
    diagram: &Diagram,
    known_claims: &HashMap<ClaimId, &Claim>,
    known_evidence: &HashSet<EvidenceId>,
) -> Result<(), EvidenceValidationError> {
    if diagram.title.trim().is_empty() {
        return Err(EvidenceValidationError::EmptyDiagramTitle);
    }
    if !(2..=32).contains(&diagram.nodes.len()) {
        return Err(EvidenceValidationError::InvalidDiagramNodeCount {
            node_count: diagram.nodes.len(),
        });
    }
    if !(1..=64).contains(&diagram.edges.len()) {
        return Err(EvidenceValidationError::InvalidDiagramEdgeCount {
            edge_count: diagram.edges.len(),
        });
    }

    let mut node_ids = HashMap::with_capacity(diagram.nodes.len());
    for (node_index, node) in diagram.nodes.iter().enumerate() {
        if node.id.trim().is_empty() {
            return Err(EvidenceValidationError::EmptyDiagramNodeId { node_index });
        }
        if node.label.trim().is_empty() {
            return Err(EvidenceValidationError::EmptyDiagramNodeLabel { node_index });
        }
        if let Some(first_node_index) = node_ids.insert(node.id.as_str(), node_index) {
            return Err(EvidenceValidationError::DuplicateDiagramNodeId {
                first_node_index,
                duplicate_node_index: node_index,
            });
        }
    }

    for (edge_index, edge) in diagram.edges.iter().enumerate() {
        if !node_ids.contains_key(edge.source.as_str()) {
            return Err(EvidenceValidationError::UnknownDiagramEdgeSource { edge_index });
        }
        if !node_ids.contains_key(edge.target.as_str()) {
            return Err(EvidenceValidationError::UnknownDiagramEdgeTarget { edge_index });
        }
    }

    for (node_index, node) in diagram.nodes.iter().enumerate() {
        validate_diagram_bindings(
            &node.claim_ids,
            &node.evidence_ids,
            known_claims,
            known_evidence,
        )
        .map_err(|error| error.for_node(node_index))?;
    }
    for (edge_index, edge) in diagram.edges.iter().enumerate() {
        validate_diagram_bindings(
            &edge.claim_ids,
            &edge.evidence_ids,
            known_claims,
            known_evidence,
        )
        .map_err(|error| error.for_edge(edge_index))?;
    }
    Ok(())
}

fn validate_diagram_bindings(
    claim_ids: &[ClaimId],
    evidence_ids: &[EvidenceId],
    known_claims: &HashMap<ClaimId, &Claim>,
    known_evidence: &HashSet<EvidenceId>,
) -> Result<(), DiagramBindingError> {
    if claim_ids.is_empty() {
        return Err(DiagramBindingError::WithoutClaims);
    }
    if evidence_ids.is_empty() {
        return Err(DiagramBindingError::WithoutEvidence);
    }
    if let Some((first_index, duplicate_index)) = duplicate_indices(claim_ids) {
        return Err(DiagramBindingError::DuplicateClaim {
            first_index,
            duplicate_index,
        });
    }
    if let Some((first_index, duplicate_index)) = duplicate_indices(evidence_ids) {
        return Err(DiagramBindingError::DuplicateEvidence {
            first_index,
            duplicate_index,
        });
    }

    let mut linked_claims = Vec::with_capacity(claim_ids.len());
    for (claim_index, claim_id) in claim_ids.iter().enumerate() {
        let Some(claim) = known_claims.get(claim_id) else {
            return Err(DiagramBindingError::UnknownClaim { claim_index });
        };
        if claim.kind != ClaimKind::Fact {
            return Err(DiagramBindingError::NonFactClaim { claim_index });
        }
        linked_claims.push(*claim);
    }

    for (evidence_index, evidence_id) in evidence_ids.iter().enumerate() {
        if !known_evidence.contains(evidence_id) {
            return Err(DiagramBindingError::UnknownEvidence { evidence_index });
        }
        if !linked_claims
            .iter()
            .any(|claim| claim.evidence_ids.contains(evidence_id))
        {
            return Err(DiagramBindingError::EvidenceNotLinkedToAnyClaim { evidence_index });
        }
    }
    for (claim_index, claim) in linked_claims.iter().enumerate() {
        if !evidence_ids
            .iter()
            .any(|evidence_id| claim.evidence_ids.contains(evidence_id))
        {
            return Err(DiagramBindingError::ClaimWithoutLinkedEvidence { claim_index });
        }
    }
    Ok(())
}

fn duplicate_indices<T>(values: &[T]) -> Option<(usize, usize)>
where
    T: Eq + Hash,
{
    let mut first_indices = HashMap::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        if let Some(first_index) = first_indices.insert(value, index) {
            return Some((first_index, index));
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiagramBindingError {
    WithoutClaims,
    WithoutEvidence,
    DuplicateClaim {
        first_index: usize,
        duplicate_index: usize,
    },
    DuplicateEvidence {
        first_index: usize,
        duplicate_index: usize,
    },
    UnknownClaim {
        claim_index: usize,
    },
    NonFactClaim {
        claim_index: usize,
    },
    UnknownEvidence {
        evidence_index: usize,
    },
    ClaimWithoutLinkedEvidence {
        claim_index: usize,
    },
    EvidenceNotLinkedToAnyClaim {
        evidence_index: usize,
    },
}

impl DiagramBindingError {
    const fn for_node(self, node_index: usize) -> EvidenceValidationError {
        match self {
            Self::WithoutClaims => EvidenceValidationError::DiagramNodeWithoutClaims { node_index },
            Self::WithoutEvidence => {
                EvidenceValidationError::DiagramNodeWithoutEvidence { node_index }
            }
            Self::DuplicateClaim {
                first_index,
                duplicate_index,
            } => EvidenceValidationError::DuplicateDiagramNodeClaim {
                node_index,
                first_claim_index: first_index,
                duplicate_claim_index: duplicate_index,
            },
            Self::DuplicateEvidence {
                first_index,
                duplicate_index,
            } => EvidenceValidationError::DuplicateDiagramNodeEvidence {
                node_index,
                first_evidence_index: first_index,
                duplicate_evidence_index: duplicate_index,
            },
            Self::UnknownClaim { claim_index } => {
                EvidenceValidationError::UnknownDiagramNodeClaim {
                    node_index,
                    claim_index,
                }
            }
            Self::NonFactClaim { claim_index } => {
                EvidenceValidationError::NonFactDiagramNodeClaim {
                    node_index,
                    claim_index,
                }
            }
            Self::UnknownEvidence { evidence_index } => {
                EvidenceValidationError::UnknownDiagramNodeEvidence {
                    node_index,
                    evidence_index,
                }
            }
            Self::ClaimWithoutLinkedEvidence { claim_index } => {
                EvidenceValidationError::DiagramNodeClaimWithoutLinkedEvidence {
                    node_index,
                    claim_index,
                }
            }
            Self::EvidenceNotLinkedToAnyClaim { evidence_index } => {
                EvidenceValidationError::DiagramNodeEvidenceNotLinkedToAnyClaim {
                    node_index,
                    evidence_index,
                }
            }
        }
    }

    const fn for_edge(self, edge_index: usize) -> EvidenceValidationError {
        match self {
            Self::WithoutClaims => EvidenceValidationError::DiagramEdgeWithoutClaims { edge_index },
            Self::WithoutEvidence => {
                EvidenceValidationError::DiagramEdgeWithoutEvidence { edge_index }
            }
            Self::DuplicateClaim {
                first_index,
                duplicate_index,
            } => EvidenceValidationError::DuplicateDiagramEdgeClaim {
                edge_index,
                first_claim_index: first_index,
                duplicate_claim_index: duplicate_index,
            },
            Self::DuplicateEvidence {
                first_index,
                duplicate_index,
            } => EvidenceValidationError::DuplicateDiagramEdgeEvidence {
                edge_index,
                first_evidence_index: first_index,
                duplicate_evidence_index: duplicate_index,
            },
            Self::UnknownClaim { claim_index } => {
                EvidenceValidationError::UnknownDiagramEdgeClaim {
                    edge_index,
                    claim_index,
                }
            }
            Self::NonFactClaim { claim_index } => {
                EvidenceValidationError::NonFactDiagramEdgeClaim {
                    edge_index,
                    claim_index,
                }
            }
            Self::UnknownEvidence { evidence_index } => {
                EvidenceValidationError::UnknownDiagramEdgeEvidence {
                    edge_index,
                    evidence_index,
                }
            }
            Self::ClaimWithoutLinkedEvidence { claim_index } => {
                EvidenceValidationError::DiagramEdgeClaimWithoutLinkedEvidence {
                    edge_index,
                    claim_index,
                }
            }
            Self::EvidenceNotLinkedToAnyClaim { evidence_index } => {
                EvidenceValidationError::DiagramEdgeEvidenceNotLinkedToAnyClaim {
                    edge_index,
                    evidence_index,
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceValidationError {
    #[error("fact claim {claim_id} has no evidence")]
    FactWithoutEvidence { claim_id: ClaimId },
    #[error("evidence {evidence_id} occurs more than once")]
    DuplicateEvidence { evidence_id: EvidenceId },
    #[error("claim {claim_id} occurs more than once")]
    DuplicateClaim { claim_id: ClaimId },
    #[error("call path {call_path_id} occurs more than once")]
    DuplicateCallPath { call_path_id: CallPathId },
    #[error("claim {claim_id} refers to unknown evidence {evidence_id}")]
    UnknownClaimEvidence {
        claim_id: ClaimId,
        evidence_id: EvidenceId,
    },
    #[error("call path {call_path_id} refers to unknown evidence {evidence_id}")]
    UnknownCallPathEvidence {
        call_path_id: CallPathId,
        evidence_id: EvidenceId,
    },
    #[error("answer offers {actual} suggested actions; at most 4 are allowed")]
    TooManySuggestedActions { actual: usize },
    #[error("answer offers the same suggested action more than once")]
    DuplicateSuggestedAction,
    #[error("suggested action refers to unknown claim {claim_id}")]
    UnknownSuggestedClaim { claim_id: ClaimId },
    #[error("suggested action refers to unknown call path {call_path_id}")]
    UnknownSuggestedCallPath { call_path_id: CallPathId },
    #[error("suggested action refers to unknown evidence {evidence_id}")]
    UnknownSuggestedEvidence { evidence_id: EvidenceId },
    #[error("diagram decision reason must not be empty")]
    EmptyDiagramDecisionReason,
    #[error("diagram title must not be empty")]
    EmptyDiagramTitle,
    #[error("diagram must contain 2..=32 nodes, got {node_count}")]
    InvalidDiagramNodeCount { node_count: usize },
    #[error("diagram must contain 1..=64 edges, got {edge_count}")]
    InvalidDiagramEdgeCount { edge_count: usize },
    #[error("diagram node at index {node_index} has an empty ID")]
    EmptyDiagramNodeId { node_index: usize },
    #[error("diagram node at index {node_index} has an empty label")]
    EmptyDiagramNodeLabel { node_index: usize },
    #[error(
        "diagram node at index {duplicate_node_index} duplicates the ID of node at index {first_node_index}"
    )]
    DuplicateDiagramNodeId {
        first_node_index: usize,
        duplicate_node_index: usize,
    },
    #[error("diagram edge at index {edge_index} has an unknown source node")]
    UnknownDiagramEdgeSource { edge_index: usize },
    #[error("diagram edge at index {edge_index} has an unknown target node")]
    UnknownDiagramEdgeTarget { edge_index: usize },
    #[error("diagram node at index {node_index} must link at least one claim")]
    DiagramNodeWithoutClaims { node_index: usize },
    #[error("diagram node at index {node_index} must link at least one evidence record")]
    DiagramNodeWithoutEvidence { node_index: usize },
    #[error(
        "diagram node at index {node_index} claim at index {duplicate_claim_index} duplicates claim at index {first_claim_index}"
    )]
    DuplicateDiagramNodeClaim {
        node_index: usize,
        first_claim_index: usize,
        duplicate_claim_index: usize,
    },
    #[error(
        "diagram node at index {node_index} evidence at index {duplicate_evidence_index} duplicates evidence at index {first_evidence_index}"
    )]
    DuplicateDiagramNodeEvidence {
        node_index: usize,
        first_evidence_index: usize,
        duplicate_evidence_index: usize,
    },
    #[error("diagram node at index {node_index} refers to an unknown claim at index {claim_index}")]
    UnknownDiagramNodeClaim {
        node_index: usize,
        claim_index: usize,
    },
    #[error("diagram node at index {node_index} refers to a non-fact claim at index {claim_index}")]
    NonFactDiagramNodeClaim {
        node_index: usize,
        claim_index: usize,
    },
    #[error(
        "diagram node at index {node_index} refers to unknown evidence at index {evidence_index}"
    )]
    UnknownDiagramNodeEvidence {
        node_index: usize,
        evidence_index: usize,
    },
    #[error(
        "diagram node at index {node_index} claim at index {claim_index} has no linked element evidence"
    )]
    DiagramNodeClaimWithoutLinkedEvidence {
        node_index: usize,
        claim_index: usize,
    },
    #[error(
        "diagram node at index {node_index} evidence at index {evidence_index} is not linked to any element claim"
    )]
    DiagramNodeEvidenceNotLinkedToAnyClaim {
        node_index: usize,
        evidence_index: usize,
    },
    #[error("diagram edge at index {edge_index} must link at least one claim")]
    DiagramEdgeWithoutClaims { edge_index: usize },
    #[error("diagram edge at index {edge_index} must link at least one evidence record")]
    DiagramEdgeWithoutEvidence { edge_index: usize },
    #[error(
        "diagram edge at index {edge_index} claim at index {duplicate_claim_index} duplicates claim at index {first_claim_index}"
    )]
    DuplicateDiagramEdgeClaim {
        edge_index: usize,
        first_claim_index: usize,
        duplicate_claim_index: usize,
    },
    #[error(
        "diagram edge at index {edge_index} evidence at index {duplicate_evidence_index} duplicates evidence at index {first_evidence_index}"
    )]
    DuplicateDiagramEdgeEvidence {
        edge_index: usize,
        first_evidence_index: usize,
        duplicate_evidence_index: usize,
    },
    #[error("diagram edge at index {edge_index} refers to an unknown claim at index {claim_index}")]
    UnknownDiagramEdgeClaim {
        edge_index: usize,
        claim_index: usize,
    },
    #[error("diagram edge at index {edge_index} refers to a non-fact claim at index {claim_index}")]
    NonFactDiagramEdgeClaim {
        edge_index: usize,
        claim_index: usize,
    },
    #[error(
        "diagram edge at index {edge_index} refers to unknown evidence at index {evidence_index}"
    )]
    UnknownDiagramEdgeEvidence {
        edge_index: usize,
        evidence_index: usize,
    },
    #[error(
        "diagram edge at index {edge_index} claim at index {claim_index} has no linked element evidence"
    )]
    DiagramEdgeClaimWithoutLinkedEvidence {
        edge_index: usize,
        claim_index: usize,
    },
    #[error(
        "diagram edge at index {edge_index} evidence at index {evidence_index} is not linked to any element claim"
    )]
    DiagramEdgeEvidenceNotLinkedToAnyClaim {
        edge_index: usize,
        evidence_index: usize,
    },
}

impl EvidenceValidationError {
    /// Whether this failure is confined to the optional diagram contract.
    #[must_use]
    pub const fn is_diagram_error(&self) -> bool {
        !matches!(
            self,
            Self::FactWithoutEvidence { .. }
                | Self::DuplicateEvidence { .. }
                | Self::DuplicateClaim { .. }
                | Self::DuplicateCallPath { .. }
                | Self::UnknownClaimEvidence { .. }
                | Self::UnknownCallPathEvidence { .. }
                | Self::TooManySuggestedActions { .. }
                | Self::DuplicateSuggestedAction
                | Self::UnknownSuggestedClaim { .. }
                | Self::UnknownSuggestedCallPath { .. }
                | Self::UnknownSuggestedEvidence { .. }
        )
    }
}
