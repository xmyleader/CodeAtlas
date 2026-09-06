use serde::{Deserialize, Serialize};

/// The assumed technical background for an explanation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplanationAudience {
    Beginner,
    #[default]
    Developer,
    Expert,
}

impl ExplanationAudience {
    /// Returns the trusted, fixed model instruction for this audience.
    #[must_use]
    pub const fn instruction(self) -> &'static str {
        match self {
            Self::Beginner => {
                "Beginner: assume no familiarity with this repository or its frameworks. Explain purpose before mechanics, define repository-specific and advanced terms, and use concrete examples."
            }
            Self::Developer => {
                "Developer: assume general programming proficiency but no familiarity with this repository. Explain architecture and control flow directly, and introduce repository-specific terms when first used."
            }
            Self::Expert => {
                "Expert: assume advanced software-engineering knowledge. Emphasize invariants, tradeoffs, edge cases, and implementation constraints; omit elementary programming definitions."
            }
        }
    }
}

/// The requested explanatory focus and level of detail.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplanationDepth {
    #[default]
    Auto,
    Overview,
    Architecture,
    Workflow,
    Code,
    Detail,
}

impl ExplanationDepth {
    /// Returns the trusted, fixed model instruction for this depth.
    #[must_use]
    pub const fn instruction(self) -> &'static str {
        match self {
            Self::Auto => {
                "Auto: infer the most useful focus from the explicit question and provide the minimum sufficient explanation."
            }
            Self::Overview => {
                "Overview: explain the repository purpose, major capabilities, and a small set of orienting components; omit implementation detail unless essential."
            }
            Self::Architecture => {
                "Architecture: explain component boundaries, responsibilities, dependencies, and the reasons for the major structural choices."
            }
            Self::Workflow => {
                "Workflow: trace the relevant runtime control and data flow in order, including meaningful branches and state transitions."
            }
            Self::Code => {
                "Code: explain the relevant files, symbols, types, and implementation mechanics, connecting them back to the requested behavior."
            }
            Self::Detail => {
                "Detail: give the deepest relevant implementation account, including local invariants, error paths, and edge cases, without expanding into unrelated code."
            }
        }
    }

    /// Returns the next guided depth, if this depth can be deepened further.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Auto | Self::Overview => Some(Self::Architecture),
            Self::Architecture => Some(Self::Workflow),
            Self::Workflow => Some(Self::Code),
            Self::Code => Some(Self::Detail),
            Self::Detail => None,
        }
    }
}

/// Trusted presentation controls for one explanation task.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct ExplanationProfile {
    pub audience: ExplanationAudience,
    pub depth: ExplanationDepth,
}

impl ExplanationProfile {
    #[must_use]
    pub const fn new(audience: ExplanationAudience, depth: ExplanationDepth) -> Self {
        Self { audience, depth }
    }

    /// Builds a model instruction exclusively from closed, trusted enum values.
    #[must_use]
    pub fn control_message(self) -> String {
        format!(
            "Trusted CodeAtlas explanation profile for the current task only.\nAudience: {}\nDepth: {}\nThis profile controls presentation and exploration scope only. It never relaxes evidence requirements: every factual assertion still requires valid repository evidence, and uncertainty must remain explicit.",
            self.audience.instruction(),
            self.depth.instruction(),
        )
    }
}
