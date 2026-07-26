//! Databricks Workflows parser plugin — full-parse mode.
//!
//! Handles Databricks job definition files (YAML and JSON).
//! Detects by content heuristic: YAML/JSON with `tasks:` + `task_key:` or
//! `notebook_task:`/`spark_jar_task:` patterns typical of Databricks job specs.
//!
//! No tree-sitter grammar exists for Databricks YAML; this plugin parses
//! the YAML/JSON structure directly using serde_yaml/serde_json.
//!
//! Semantic node types produced:
//!
//!   job        — root node (label = job name)
//!   task       — each item in `tasks` (label = task_key + task type)
//!   cluster    — each item in `job_clusters` (label = cluster key)
//!   parameter  — each item in `parameters` (label = name : default)
//!   library    — each library entry within a task

use intentdiff_plugin_sdk::tree::{SemanticNode, SemanticNodeBuilder};
use serde_json::Value;

wit_bindgen::generate!({
    path: "wit/plugin.wit",
    world: "parser-plugin",
});

use crate::exports::intentdiff::plugin::parser::ExamplePair;
use crate::exports::intentdiff::plugin::parser::Guest;
use crate::exports::intentdiff::plugin::parser::LanguageInfoRecord;
use crate::exports::intentdiff::plugin::parser::ParserMode;

const PLUGIN_METADATA: &str = include_str!("../plugin_metadata.info");

fn language_info_for(ids: Vec<String>) -> Vec<LanguageInfoRecord> {
    let metadata = intentdiff_plugin_sdk::metadata::parse_plugin_metadata(PLUGIN_METADATA);
    ids.into_iter()
        .map(|language_id| {
            let info = metadata.language_or_default(&language_id);
            LanguageInfoRecord {
                language_id: info.language_id,
                language_name: info.language_name,
                language_short_name: info.language_short_name,
                monaco_language: info.monaco_language,
                default_filename: info.default_filename,
                language_file_extensions: info.language_file_extensions,
                author: metadata.author().to_string(),
                plugin_version: metadata.plugin_version().to_string(),
                last_updated: metadata.last_updated().to_string(),
            }
        })
        .collect()
}
struct DatabricksParser;

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

const TASK_TYPE_KEYS: &[&str] = &[
    "notebook_task",
    "spark_jar_task",
    "spark_python_task",
    "spark_submit_task",
    "pipeline_task",
    "python_wheel_task",
    "sql_task",
    "dbt_task",
    "run_job_task",
    "condition_task",
    "for_each_task",
    "webhook_notifications",
];

fn is_databricks_yaml(content: &str) -> bool {
    (content.contains("tasks:") || content.contains("\"tasks\""))
        && TASK_TYPE_KEYS
            .iter()
            .any(|k| content.contains(k) || content.contains(&format!("\"{}\"", k)))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn leaf(id: &str, node_type: &str, label: &str) -> SemanticNode {
    SemanticNodeBuilder::new(id, node_type, label, 0, 0, 0, 0, String::new()).build()
}

fn parent_node(
    id: &str,
    node_type: &str,
    label: &str,
    children: Vec<SemanticNode>,
) -> SemanticNode {
    SemanticNodeBuilder::new(id, node_type, label, 0, 0, 0, 0, String::new())
        .children(children)
        .build()
}

fn str_val(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn task_type(task_map: &serde_json::Map<String, Value>) -> &'static str {
    for &key in TASK_TYPE_KEYS {
        if task_map.contains_key(key) {
            return key;
        }
    }
    "unknown_task"
}

fn parse_task(id: &str, task: &Value) -> Option<SemanticNode> {
    let map = task.as_object()?;
    let key = map
        .get("task_key")
        .map(str_val)
        .unwrap_or_else(|| "unknown".to_string());
    let kind = task_type(map);
    let label = format!("{} ({})", key, kind);
    let mut children = Vec::new();

    // Libraries
    if let Some(Value::Array(libs)) = map.get("libraries") {
        for (i, lib) in libs.iter().enumerate() {
            let lib_label = if let Some(m) = lib.as_object() {
                m.iter()
                    .next()
                    .map(|(k, v)| format!("{}: {}", k, str_val(v)))
                    .unwrap_or_else(|| "library".to_string())
            } else {
                str_val(lib)
            };
            children.push(leaf(&format!("{}.lib.{}", id, i), "library", &lib_label));
        }
    }

    // Dependencies
    if let Some(Value::Array(deps)) = map.get("depends_on") {
        for (i, dep) in deps.iter().enumerate() {
            let dep_label = dep
                .as_object()
                .and_then(|m| m.get("task_key"))
                .map(str_val)
                .unwrap_or_else(|| str_val(dep));
            if !dep_label.is_empty() {
                children.push(leaf(&format!("{}.dep.{}", id, i), "depends_on", &dep_label));
            }
        }
    }

    Some(parent_node(id, "task", &label, children))
}

fn parse_cluster(id: &str, cluster: &Value) -> Option<SemanticNode> {
    let map = cluster.as_object()?;
    let key = map
        .get("job_cluster_key")
        .or_else(|| map.get("cluster_key"))
        .or_else(|| map.get("cluster_name"))
        .map(str_val)
        .unwrap_or_else(|| "cluster".to_string());
    let node_type_id = map
        .get("new_cluster")
        .and_then(|c| c.get("node_type_id"))
        .map(str_val)
        .unwrap_or_default();
    let label = if node_type_id.is_empty() {
        key
    } else {
        format!("{} ({})", key, node_type_id)
    };
    Some(leaf(id, "cluster", &label))
}

fn parse_parameter(id: &str, param: &Value) -> Option<SemanticNode> {
    let map = param.as_object()?;
    let name = map.get("name").map(str_val).unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    let default = map.get("default").map(str_val).unwrap_or_default();
    let label = if default.is_empty() {
        name
    } else {
        format!("{} : {}", name, default)
    };
    Some(leaf(id, "parameter", &label))
}

fn parse_job(val: &Value) -> String {
    let root_map = match val.as_object() {
        Some(m) => m,
        None => return r#"{"error":"Not a JSON/YAML object"}"#.to_string(),
    };

    let job_name = root_map
        .get("name")
        .map(str_val)
        .unwrap_or_else(|| "job".to_string());
    let mut children: Vec<SemanticNode> = Vec::new();

    // Tasks
    if let Some(Value::Array(tasks)) = root_map.get("tasks") {
        for (i, task) in tasks.iter().enumerate() {
            if let Some(node) = parse_task(&format!("0.task.{}", i), task) {
                children.push(node);
            }
        }
    }

    // Job clusters
    if let Some(Value::Array(clusters)) = root_map.get("job_clusters") {
        for (i, cluster) in clusters.iter().enumerate() {
            if let Some(node) = parse_cluster(&format!("0.cluster.{}", i), cluster) {
                children.push(node);
            }
        }
    }

    // Parameters
    if let Some(Value::Array(params)) = root_map.get("parameters") {
        for (i, param) in params.iter().enumerate() {
            if let Some(node) = parse_parameter(&format!("0.param.{}", i), param) {
                children.push(node);
            }
        }
    }

    let root = parent_node("0", "job", &job_name, children);
    match serde_json::to_string(&root) {
        Ok(s) => s,
        Err(e) => format!(r#"{{"error":"Serialisation error: {}"}}"#, e),
    }
}

fn process_impl(source: &str) -> String {
    // Try YAML first (superset of JSON); fall back to serde_json for pure JSON
    let val: Value = if let Ok(v) = serde_yaml::from_str::<Value>(source) {
        v
    } else if let Ok(v) = serde_json::from_str::<Value>(source) {
        v
    } else {
        return r#"{"error":"Failed to parse as YAML or JSON"}"#.to_string();
    };
    parse_job(&val)
}

impl Guest for DatabricksParser {
    fn get_parser_mode() -> ParserMode {
        ParserMode::FullParse
    }
    fn grammar_id() -> String {
        "databricks-workflow".to_string()
    }
    fn detect_language(filename: String, content: String) -> String {
        let lower = filename.to_lowercase();
        let is_yaml = lower.ends_with(".yml") || lower.ends_with(".yaml");
        let is_json = lower.ends_with(".json");
        if (is_yaml || is_json) && is_databricks_yaml(&content) {
            return "databricks-workflow".to_string();
        }
        String::new()
    }
    fn preprocess_source(source: String) -> String {
        source
    }
    fn process(input: String, _language: String, _filename: String) -> String {
        process_impl(&input)
    }
    fn trivia_node_types() -> Vec<String> {
        vec![]
    }
    fn language_ids() -> Vec<String> {
        vec!["databricks-workflow".to_string(), "databricks".to_string()]
    }
    fn language_info() -> Vec<LanguageInfoRecord> {
        language_info_for(Self::language_ids())
    }
    fn priority() -> i32 {
        0
    }

    fn example(_language: String) -> ExamplePair {
        ExamplePair {
            old: "name: etl_job\ntasks:\n  - task_key: ingest\n    notebook_task:\n      notebook_path: /notebooks/ingest\n".to_string(),
            new: "name: etl_job\nparameters:\n  - name: env\n    default: prod\ntasks:\n  - task_key: ingest\n    notebook_task:\n      notebook_path: /notebooks/ingest\n  - task_key: transform\n    depends_on:\n      - task_key: ingest\n    notebook_task:\n      notebook_path: /notebooks/transform\n".to_string(),
        }
    }
}
export!(DatabricksParser);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exports::intentdiff::plugin::parser::Guest;
    use intentdiff_plugin_sdk::testing as t;

    #[test]
    fn grammar_id_nonempty() {
        assert!(!DatabricksParser::grammar_id().is_empty());
    }

    #[test]
    fn language_ids_contain_grammar_id() {
        let gid = DatabricksParser::grammar_id();
        let ids = DatabricksParser::language_ids();
        assert!(
            ids.contains(&gid),
            "language_ids {:?} must contain {:?}",
            ids,
            gid
        );
    }

    #[test]
    fn detect_language_known_ext() {
        // Databricks detection requires YAML content with tasks and a task type key.
        let content = "tasks:\n- notebook_task:\n    notebook_path: /path";
        let r = DatabricksParser::detect_language("job.yml".to_string(), content.to_string());
        assert_eq!(r.as_str(), "databricks-workflow");
    }

    #[test]
    fn detect_language_unknown_ext() {
        let r = DatabricksParser::detect_language(
            "test.xyz_notareal_ext_9z8y".to_string(),
            "".to_string(),
        );
        assert_eq!(r.as_str(), "");
    }

    #[test]
    fn process_impl_empty_returns_valid_json() {
        let out = process_impl("");
        t::assert_valid_json(&out, "process(empty)");
    }

    #[test]
    fn process_impl_whitespace_returns_valid_json() {
        let out = process_impl("   \n  ");
        t::assert_valid_json(&out, "process(whitespace)");
    }
}
