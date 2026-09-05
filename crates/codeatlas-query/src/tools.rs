use std::sync::Arc;

use async_trait::async_trait;
use codeatlas_core::{ToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{QueryError, RepositoryIndex};

pub const TOOL_NAMES: [&str; 9] = [
    "list_files",
    "read_file",
    "find_symbol",
    "find_references",
    "get_symbol",
    "get_module",
    "search_code",
    "trace_call",
    "get_repository_overview",
];

/// Tool adapter exposing repository queries through the core executor contract.
#[derive(Debug, Clone)]
pub struct RepositoryTools {
    index: Arc<RepositoryIndex>,
}

impl RepositoryTools {
    #[must_use]
    pub fn new(index: RepositoryIndex) -> Self {
        Self {
            index: Arc::new(index),
        }
    }

    #[must_use]
    pub const fn from_shared(index: Arc<RepositoryIndex>) -> Self {
        Self { index }
    }

    #[must_use]
    pub fn index(&self) -> &RepositoryIndex {
        &self.index
    }

    #[must_use]
    pub fn definitions() -> Vec<ToolDefinition> {
        definitions()
    }

    fn dispatch(&self, name: &str, arguments: &Value) -> Result<Value, ToolError> {
        match name {
            "list_files" => self.run(name, arguments, RepositoryIndex::list_files),
            "read_file" => self.run(name, arguments, RepositoryIndex::read_file),
            "find_symbol" => self.run(name, arguments, RepositoryIndex::find_symbol),
            "find_references" => self.run(name, arguments, RepositoryIndex::find_references),
            "get_symbol" => self.run(name, arguments, RepositoryIndex::get_symbol),
            "get_module" => self.run(name, arguments, RepositoryIndex::get_module),
            "search_code" => self.run(name, arguments, RepositoryIndex::search_code),
            "trace_call" => self.run(name, arguments, RepositoryIndex::trace_call),
            "get_repository_overview" => {
                self.run(name, arguments, RepositoryIndex::get_repository_overview)
            }
            _ => Err(ToolError::NotFound {
                name: name.to_owned(),
            }),
        }
    }

    fn run<Q, R>(
        &self,
        name: &str,
        arguments: &Value,
        query: impl FnOnce(&RepositoryIndex, &Q) -> Result<R, QueryError>,
    ) -> Result<Value, ToolError>
    where
        Q: DeserializeOwned,
        R: serde::Serialize,
    {
        let arguments = parse_arguments(name, arguments)?;
        let result =
            query(&self.index, &arguments).map_err(|error| map_query_error(name, error))?;
        serde_json::to_value(result).map_err(|error| ToolError::Execution {
            name: name.to_owned(),
            message: format!("failed to serialize query result: {error}"),
        })
    }
}

#[async_trait]
impl ToolExecutor for RepositoryTools {
    async fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        let tools = self.clone();
        let call = call.clone();
        let name = call.name.clone();
        tokio::task::spawn_blocking(move || {
            let result = tools.dispatch(&call.name, &call.arguments)?;
            Ok(ToolOutput {
                call_id: call.id,
                result,
                is_error: false,
            })
        })
        .await
        .map_err(|error| ToolError::Execution {
            name,
            message: format!("blocking task failed: {error}"),
        })?
    }
}

fn parse_arguments<T: DeserializeOwned>(name: &str, arguments: &Value) -> Result<T, ToolError> {
    serde_json::from_value(arguments.clone()).map_err(|error| ToolError::InvalidArguments {
        name: name.to_owned(),
        message: error.to_string(),
    })
}

fn map_query_error(name: &str, error: QueryError) -> ToolError {
    match error {
        QueryError::InvalidQuery { message } => ToolError::InvalidArguments {
            name: name.to_owned(),
            message,
        },
        error => ToolError::Execution {
            name: name.to_owned(),
            message: error.to_string(),
        },
    }
}

/// Returns the deterministic definitions for all repository tools.
#[must_use]
pub fn definitions() -> Vec<ToolDefinition> {
    vec![
        definition(
            "list_files",
            "List repository-model files in stable path order.",
            object_schema(
                &json!({
                    "prefix": path_schema("Optional repository-relative path prefix."),
                    "limit": limit_schema()
                }),
                &[],
            ),
        ),
        definition(
            "read_file",
            "Read a line range from an indexed text file. Large ranges return a bounded page with truncated=true instead of failing; continue from the returned end_line when more source is needed.",
            object_schema(
                &json!({
                    "path": path_schema("Indexed repository-relative file path."),
                    "start_line": {"type": "integer", "minimum": 1},
                    "end_line": {"type": "integer", "minimum": 1}
                }),
                &["path"],
            ),
        ),
        definition(
            "find_symbol",
            "Find definitions by simple or qualified symbol name.",
            search_input_schema(false),
        ),
        definition(
            "find_references",
            "Find explicit and conservatively resolved references to a symbol.",
            object_schema(
                &json!({"symbol_id": id_schema(), "limit": limit_schema()}),
                &["symbol_id"],
            ),
        ),
        definition(
            "get_symbol",
            "Get a symbol definition and bounded references/call relationships.",
            object_schema(&json!({"symbol_id": id_schema()}), &["symbol_id"]),
        ),
        definition(
            "get_module",
            "Get a module by ID or exact qualified name.",
            module_input_schema(),
        ),
        definition(
            "search_code",
            "Perform bounded literal text search over indexed source files.",
            search_input_schema(true),
        ),
        definition(
            "trace_call",
            "Trace callers or callees with cycle-safe depth and node bounds.",
            object_schema(
                &json!({
                    "symbol_id": id_schema(),
                    "direction": {"type": "string", "enum": ["callers", "callees"]},
                    "max_depth": {"type": "integer", "minimum": 0, "maximum": 20},
                    "max_nodes": {"type": "integer", "minimum": 1, "maximum": 1000}
                }),
                &["symbol_id", "direction"],
            ),
        ),
        definition(
            "get_repository_overview",
            "Return a compact repository summary with bounded modules, entry points, and evidence.",
            object_schema(
                &json!({
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "description": "Maximum modules and entry points; defaults to 12."
                    },
                    "include_tests": {
                        "type": "boolean",
                        "description": "Include test and benchmark entry points; defaults to false."
                    }
                }),
                &[],
            ),
        ),
    ]
}

fn definition(name: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
        output_schema: Some(envelope_schema()),
    }
}

fn object_schema(properties: &Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn search_input_schema(with_path: bool) -> Value {
    let mut properties = serde_json::Map::from_iter([
        (
            "query".to_owned(),
            json!({"type": "string", "minLength": 1, "maxLength": 256}),
        ),
        ("case_sensitive".to_owned(), json!({"type": "boolean"})),
        ("limit".to_owned(), limit_schema()),
    ]);
    if with_path {
        properties.insert(
            "path_prefix".to_owned(),
            path_schema("Optional repository-relative path prefix."),
        );
    }
    object_schema(&Value::Object(properties), &["query"])
}

fn module_input_schema() -> Value {
    let mut schema = object_schema(
        &json!({
            "module_id": id_schema(),
            "qualified_name": {"type": "string", "minLength": 1, "maxLength": 256},
            "limit": limit_schema()
        }),
        &[],
    );
    schema["oneOf"] = json!([
        {"required": ["module_id"], "not": {"required": ["qualified_name"]}},
        {"required": ["qualified_name"], "not": {"required": ["module_id"]}}
    ]);
    schema
}

fn path_schema(description: &str) -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "description": description,
        "pattern": "^(?!/)(?![A-Za-z]:)(?!.*(?:^|/)\\.{1,2}(?:/|$))(?!.*\\\\).+$"
    })
}

fn id_schema() -> Value {
    json!({"type": "string", "pattern": "^[0-9a-fA-F]{32}$"})
}

fn limit_schema() -> Value {
    json!({"type": "integer", "minimum": 1, "maximum": 500})
}

fn envelope_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "data": {},
            "evidence": {
                "type": "array",
                "items": evidence_schema()
            }
        },
        "required": ["data", "evidence"],
        "additionalProperties": false
    })
}

fn evidence_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": id_schema(),
            "file_id": id_schema(),
            "path": {"type": "string"},
            "span": {
                "type": "object",
                "properties": {
                    "start": position_schema(),
                    "end": position_schema()
                },
                "required": ["start", "end"],
                "additionalProperties": false
            },
            "symbol_id": {"anyOf": [id_schema(), {"type": "null"}]},
            "excerpt": {"anyOf": [{"type": "string", "maxLength": 2048}, {"type": "null"}]}
        },
        "required": ["id", "file_id", "path", "span", "symbol_id", "excerpt"],
        "additionalProperties": false
    })
}

fn position_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "line": {"type": "integer", "minimum": 1},
            "column": {"type": "integer", "minimum": 0}
        },
        "required": ["line", "column"],
        "additionalProperties": false
    })
}
