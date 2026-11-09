//! Databricks domain selection; generic typed-node conversion lives in the SDK.
use crate::source_spans::SourceSpans;
use intentumdiff_plugin_sdk::{
    config::{ConfigEntry, ConfigKind, ConfigValue, ConfigurationProjector},
    tree::{Position, SemanticNode, SemanticNodeBuilder},
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub(crate) fn append<'source>(
    root: &mut SemanticNode,
    value: &Value,
    source: &'source str,
    spans: &mut SourceSpans<'source>,
) -> Result<(), String> {
    Context {
        spans,
        projector: ConfigurationProjector::new(source),
    }
    .domain(root, value, &[])
}

struct Context<'index, 'source> {
    spans: &'index mut SourceSpans<'source>,
    projector: ConfigurationProjector<'source>,
}

impl Context<'_, '_> {
    fn position(&mut self, path: &[String]) -> Result<Position, String> {
        let path: Vec<_> = path.iter().map(String::as_str).collect();
        self.spans
            .position(&path)
            .ok_or_else(|| format!("Missing configuration source span for {}", path.join("/")))
    }

    fn atom(&mut self, path: &[String]) -> Result<String, String> {
        let path: Vec<_> = path.iter().map(String::as_str).collect();
        self.spans
            .scalar_text(&path)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| format!("Missing exact configuration number for {}", path.join("/")))
    }

    fn key_position(&mut self, path: &[String]) -> Result<Position, String> {
        let path: Vec<_> = path.iter().map(String::as_str).collect();
        self.spans.key_position(&path).ok_or_else(|| {
            format!(
                "Missing exact configuration key span for {}",
                path.join("/")
            )
        })
    }

    fn value(
        &mut self,
        value: &Value,
        path: &[String],
        depth: usize,
    ) -> Result<ConfigValue, String> {
        if depth > 128 {
            return Err("Configuration source nesting limit exceeded".into());
        }
        let position = self.position(path)?;
        let kind = match value {
            Value::Null => ConfigKind::Null,
            Value::Bool(value) => ConfigKind::Boolean(*value),
            Value::String(value) => ConfigKind::String(value.clone()),
            // The serde_yaml/serde_json AST can already have rounded a float.
            // The source index retains the actual scalar atom independently.
            Value::Number(_) => ConfigKind::Number(self.atom(path)?),
            Value::Array(values) => {
                let mut children = Vec::with_capacity(values.len());
                for (i, value) in values.iter().enumerate() {
                    children.push(self.value(
                        value,
                        &child_path(path, &i.to_string()),
                        depth + 1,
                    )?);
                }
                ConfigKind::Sequence(children)
            }
            Value::Object(values) => {
                let mut children = Vec::with_capacity(values.len());
                for (key, value) in values {
                    let entry_path = child_path(path, key);
                    children.push(ConfigEntry {
                        key: key.clone(),
                        key_position: self.key_position(&entry_path)?,
                        value: self.value(value, &entry_path, depth + 1)?,
                    });
                }
                ConfigKind::Mapping(children)
            }
        };
        Ok(ConfigValue { position, kind })
    }

    fn field(
        &mut self,
        node: &mut SemanticNode,
        name: &str,
        value: &Value,
        path: &[String],
    ) -> Result<(), String> {
        let value = self.value(value, path, 0)?;
        let key_position = self.key_position(path)?;
        self.projected(node, name, &key_position, &value)
    }

    fn projected(
        &self,
        node: &mut SemanticNode,
        name: &str,
        key_position: &Position,
        value: &ConfigValue,
    ) -> Result<(), String> {
        let prefix = format!("{}.config.{}", node.id, node.children.len());
        node.children.push(
            self.projector
                .configuration_node(&prefix, name, key_position, value)
                .map_err(|error| format!("Configuration projection failed: {error}"))?,
        );
        Ok(())
    }

    // An injective domain label already carries this value. Keep its type and
    // key presence without duplicating the value as another review fact.
    fn marker(
        &mut self,
        node: &mut SemanticNode,
        name: &str,
        kind: &str,
        path: &[String],
    ) -> Result<(), String> {
        let p = self.key_position(path)?;
        node.children.push(
            SemanticNodeBuilder::new(
                format!("{}.config.{}", node.id, node.children.len()),
                kind,
                name,
                p.start_line,
                p.start_col,
                p.end_line,
                p.end_col,
                String::new(),
            )
            .build(),
        );
        Ok(())
    }

    fn domain(
        &mut self,
        node: &mut SemanticNode,
        value: &Value,
        path: &[String],
    ) -> Result<(), String> {
        let Some(map) = value.as_object() else {
            // Legacy library/dependency leaves also accept scalar array items.
            // The surrounding collection key is the real field token; the
            // scalar child keeps the item's own exact value position.
            let (_, collection_path) = path
                .split_last()
                .ok_or("Missing scalar configuration collection")?;
            let field = collection_path
                .last()
                .ok_or("Missing scalar configuration field")?;
            let value = self.value(value, path, 0)?;
            let key_position = self.key_position(collection_path)?;
            self.projected(node, field, &key_position, &value)?;
            return rebuild(node);
        };
        let old_children = std::mem::take(&mut node.children);
        let mut old: HashMap<_, _> = old_children
            .into_iter()
            .map(|child| (child.id.clone(), child))
            .collect();
        let collections: &[(&str, &str)] = match node.node_type.as_str() {
            "job" => &[
                ("tasks", "task"),
                ("job_clusters", "cluster"),
                ("parameters", "param"),
            ],
            "task" => &[("libraries", "lib"), ("depends_on", "dep")],
            _ => &[],
        };
        let mut represented = HashSet::new();
        let mut arrays = Vec::new();
        // Preserve the existing flat domain children and their IDs. Unmodeled
        // array items are interleaved at their real indices, never discarded.
        for &(field, segment) in collections {
            let Some(Value::Array(values)) = map.get(field) else {
                continue;
            };
            represented.insert(field);
            let array_path = child_path(path, field);
            for (i, value) in values.iter().enumerate() {
                let id = format!("{}.{}.{}", node.id, segment, i);
                let item_path = child_path(&array_path, &i.to_string());
                if let Some(mut child) = old.remove(&id) {
                    self.domain(&mut child, value, &item_path)?;
                    node.children.push(child);
                } else {
                    let value = self.value(value, &item_path, 0)?;
                    let key_position = self.key_position(&array_path)?;
                    node.children.push(
                        self.projector
                            .configuration_node(
                                &format!("{id}.config"),
                                field,
                                &key_position,
                                &value,
                            )
                            .map_err(|error| format!("Configuration projection failed: {error}"))?,
                    );
                }
            }
            arrays.push((field, array_path));
        }
        if !old.is_empty() {
            return Err("Unmapped Databricks domain children".into());
        }

        let selected_cluster_key = ["job_cluster_key", "cluster_key", "cluster_name"]
            .into_iter()
            .find(|key| map.contains_key(*key));
        let library_key = if node.node_type == "library" {
            map.keys().next().map(String::as_str)
        } else {
            None
        };

        // Markers come after the old domain children, retaining their familiar
        // ordering and anchors while distinguishing missing and empty arrays.
        for (field, array_path) in arrays {
            self.marker(node, field, "array_field", &array_path)?;
        }
        for (key, value) in map {
            if represented.contains(key.as_str()) {
                continue;
            }
            let field_path = child_path(path, key);
            let labelled_string = value.is_string()
                && match node.node_type.as_str() {
                    "job" => key == "name",
                    "task" => key == "task_key",
                    "cluster" => selected_cluster_key == Some(key.as_str()),
                    "parameter" => key == "name",
                    "library" => library_key == Some(key.as_str()),
                    "depends_on" => key == "task_key",
                    _ => false,
                };
            if labelled_string {
                self.marker(node, key, "string_field", &field_path)?;
                continue;
            }
            if node.node_type == "parameter" && key == "default" {
                if value.is_number() {
                    let name = map.get("name").and_then(Value::as_str).unwrap_or("");
                    node.label = format!("{} : {}", name, self.atom(&field_path)?);
                }
            }
            // Parameter defaults and new_cluster.node_type_id are independent
            // components of compound display labels. Retain their typed values:
            // delimiter concatenation alone can give distinct settings the same
            // human label. Ordinary diffing then selects precise scalar anchors.
            self.field(node, key, value, &field_path)?;
        }
        rebuild(node)
    }
}

fn child_path(path: &[String], part: &str) -> Vec<String> {
    let mut child = path.to_vec();
    child.push(part.to_owned());
    child
}

fn rebuild(node: &mut SemanticNode) -> Result<(), String> {
    let p = &node.position;
    let mut builder = SemanticNodeBuilder::new(
        &node.id,
        &node.node_type,
        &node.label,
        p.start_line,
        p.start_col,
        p.end_line,
        p.end_col,
        String::new(),
    )
    .children(std::mem::take(&mut node.children));
    if let Some(parent) = node.parent_type.take() {
        builder = builder.parent_type(parent);
    }
    if let Some(facts) = node.facts.take() {
        builder = builder.facts(facts);
    }
    *node = builder.build();
    Ok(())
}
