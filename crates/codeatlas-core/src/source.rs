use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeStruct};
use thiserror::Error;

/// A source position with a 1-based line and 0-based UTF-8 byte column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourcePosition {
    line: u32,
    column: u32,
}

impl SourcePosition {
    /// Creates a source position.
    ///
    /// # Errors
    ///
    /// Returns [`SourceSpanError::ZeroLine`] when `line` is zero.
    pub const fn new(line: u32, column: u32) -> Result<Self, SourceSpanError> {
        if line == 0 {
            return Err(SourceSpanError::ZeroLine);
        }
        Ok(Self { line, column })
    }

    #[must_use]
    pub const fn line(self) -> u32 {
        self.line
    }

    #[must_use]
    pub const fn column(self) -> u32 {
        self.column
    }
}

impl Serialize for SourcePosition {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("SourcePosition", 2)?;
        state.serialize_field("line", &self.line)?;
        state.serialize_field("column", &self.column)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for SourcePosition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct PositionDto {
            line: u32,
            column: u32,
        }

        let dto = PositionDto::deserialize(deserializer)?;
        Self::new(dto.line, dto.column).map_err(de::Error::custom)
    }
}

/// A validated half-open source range: `[start, end)`.
///
/// Lines are 1-based for direct user display. Columns are 0-based UTF-8 byte
/// offsets within their lines, matching common parser APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    start: SourcePosition,
    end: SourcePosition,
}

impl SourceSpan {
    /// Creates a span from line and column components.
    ///
    /// # Errors
    ///
    /// Returns [`SourceSpanError`] for a zero line or an end before the start.
    pub const fn new(
        start_line: u32,
        start_column: u32,
        end_line: u32,
        end_column: u32,
    ) -> Result<Self, SourceSpanError> {
        let start = match SourcePosition::new(start_line, start_column) {
            Ok(position) => position,
            Err(error) => return Err(error),
        };
        let end = match SourcePosition::new(end_line, end_column) {
            Ok(position) => position,
            Err(error) => return Err(error),
        };
        Self::from_positions(start, end)
    }

    /// Creates a span from validated positions.
    ///
    /// # Errors
    ///
    /// Returns [`SourceSpanError::EndBeforeStart`] when `end < start`.
    pub const fn from_positions(
        start: SourcePosition,
        end: SourcePosition,
    ) -> Result<Self, SourceSpanError> {
        if end.line < start.line || (end.line == start.line && end.column < start.column) {
            return Err(SourceSpanError::EndBeforeStart { start, end });
        }
        Ok(Self { start, end })
    }

    #[must_use]
    pub const fn start(self) -> SourcePosition {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> SourcePosition {
        self.end
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start.line == self.end.line && self.start.column == self.end.column
    }
}

impl Serialize for SourceSpan {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("SourceSpan", 2)?;
        state.serialize_field("start", &self.start)?;
        state.serialize_field("end", &self.end)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for SourceSpan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SpanDto {
            start: SourcePosition,
            end: SourcePosition,
        }

        let dto = SpanDto::deserialize(deserializer)?;
        Self::from_positions(dto.start, dto.end).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SourceSpanError {
    #[error("source line numbers are 1-based and must not be zero")]
    ZeroLine,
    #[error("source span end {end:?} is before start {start:?}")]
    EndBeforeStart {
        start: SourcePosition,
        end: SourcePosition,
    },
}
