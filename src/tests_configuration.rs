use super::*;
use std::collections::HashSet;

fn slice<'a>(source: &'a str, position: &intentumdiff_plugin_sdk::tree::Position) -> &'a str {
    let starts: Vec<_> = std::iter::once(0)
        .chain(
            source
                .bytes()
                .enumerate()
                .filter_map(|(i, b)| (b == b'\n').then_some(i + 1)),
        )
        .collect();
    let start = starts[position.start_line as usize] + position.start_col as usize;
    let end = starts[position.end_line as usize] + position.end_col as usize;
    &source[start..end]
}
#[test]
fn configuration_keys_anchor_the_actual_key_token_not_an_unchanged_value() {
    let sources = [
        r#"{"name":"etl","tasks":[],"future":{"f":1,"a.b":{"a/b":"x"},"a\"b":2,"雪🚀":3,"":4}}"#,
        "name: etl\ntasks: []\nfuture:\n  f: 1\n  'a.b':\n    \"a/b\": \"x\"\n  'a''b': 2\n  雪🚀: 3\n  '': 4\n",
        r#"{"name":"\uD83D\uDE80","tasks":[],"future":{"a\"b":1,"c\\d":2,"\uD83D\uDE80":3}}"#,
    ];
    fn check(node: &SemanticNode, source: &str) {
        if node.node_type == "property" || node.node_type.ends_with("_field") {
            let token = slice(source, &node.position);
            let decoded: String = serde_json::from_str(token)
                .ok()
                .or_else(|| serde_yaml::from_str(token).ok())
                .unwrap_or_else(|| panic!("key {:?} token {:?}", node.label, token));
            assert_eq!(decoded, node.label, "key anchored to value: {}", node.id);
        }
        for child in &node.children {
            check(child, source);
        }
    }
    for source in sources {
        let node = tree(source);
        check(&node, source);
    }
    let (old, new) = pair(
        r#"{"name":"etl","tasks":[],"future":{"f":1}}"#,
        r#"{"name":"etl","tasks":[],"future":{"g":1}}"#,
    );
    fn property<'a>(node: &'a SemanticNode, name: &str) -> Option<&'a SemanticNode> {
        if node.node_type == "property" && node.label == name {
            return Some(node);
        }
        node.children.iter().find_map(|n| property(n, name))
    }
    assert_eq!(
        slice(
            r#"{"name":"etl","tasks":[],"future":{"f":1}}"#,
            &property(&old, "f").unwrap().position
        ),
        r#""f""#
    );
    assert_eq!(
        slice(
            r#"{"name":"etl","tasks":[],"future":{"g":1}}"#,
            &property(&new, "g").unwrap().position
        ),
        r#""g""#
    );
    for (old, new, old_key, new_key, old_token, new_token) in [
        (
            r#"{"name":"etl","tasks":[],"future":{"雪🚀":1}}"#,
            r#"{"name":"etl","tasks":[],"future":{"雨🚀":1}}"#,
            "雪🚀",
            "雨🚀",
            r#""雪🚀""#,
            r#""雨🚀""#,
        ),
        (
            r#"{"name":"etl","tasks":[],"future":{"a\"b":1}}"#,
            r#"{"name":"etl","tasks":[],"future":{"c\\d":1}}"#,
            "a\"b",
            "c\\d",
            r#""a\"b""#,
            r#""c\\d""#,
        ),
        (
            r#"{"name":"etl","tasks":[],"future":{"":1}}"#,
            r#"{"name":"etl","tasks":[],"future":{"filled":1}}"#,
            "",
            "filled",
            r#""""#,
            r#""filled""#,
        ),
        (
            "name: etl\ntasks: []\nfuture:\n  雪🚀: 1\n",
            "name: etl\ntasks: []\nfuture:\n  雨🚀: 1\n",
            "雪🚀",
            "雨🚀",
            "雪🚀",
            "雨🚀",
        ),
        (
            "name: etl\ntasks: []\nfuture:\n  'a''b': 1\n",
            "name: etl\ntasks: []\nfuture:\n  \"c\\\\d\": 1\n",
            "a'b",
            "c\\d",
            "'a''b'",
            "\"c\\\\d\"",
        ),
        (
            "name: etl\ntasks: []\nfuture:\n  '': 1\n",
            "name: etl\ntasks: []\nfuture:\n  filled: 1\n",
            "",
            "filled",
            "''",
            "filled",
        ),
    ] {
        let (before, after) = pair(old, new);
        assert_eq!(
            slice(old, &property(&before, old_key).unwrap().position),
            old_token
        );
        assert_eq!(
            slice(new, &property(&after, new_key).unwrap().position),
            new_token
        );
        check(&before, old);
        check(&after, new);
    }
}
#[test]
fn numeric_and_string_payload_leaves_keep_their_own_raw_tokens() {
    for source in [
        r#"{"name":"etl","tasks":[],"future":{"exact":0.100000000000000000001,"text":"λ \"雪🚀\""}}"#,
        "name: etl\ntasks: []\nfuture:\n  exact: 0.100000000000000000001 # units\n  text: 'λ ''雪🚀'''\n",
    ] {
        let node=tree(source);
        fn check(node:&SemanticNode,source:&str) {
            if node.node_type=="number" && node.label=="0.100000000000000000001" {
                assert_eq!(slice(source,&node.position),node.label);
            }
            if node.node_type=="string" && node.label.contains("雪🚀") {
                let raw=slice(source,&node.position);
                let decoded:String=serde_yaml::from_str(raw).unwrap();
                assert_eq!(decoded,node.label);
            }
            for child in &node.children {check(child,source);}
        }
        check(&node,source);
        assert!(labels(&node).contains(&"0.100000000000000000001"));
    }
}

#[test]
fn yaml_alias_mapping_keys_use_the_actual_alias_occurrence() {
    for source in [
        "name: etl\ntasks: []\nkey: &k a\nfuture:\n  *k : 1",
        "name: etl\r\ntasks: []\r\nkey: &k 雪🚀\r\nfuture:\r\n  *k : 1\r\n",
    ] {
        let node = tree(source);
        valid(&node, source, &mut HashSet::new());
        fn find(node: &SemanticNode) -> Option<&SemanticNode> {
            if node.node_type == "property" && (node.label == "a" || node.label == "雪🚀") {
                return Some(node);
            }
            node.children.iter().find_map(find)
        }
        let key = find(&node).unwrap();
        assert_eq!(slice(source, &key.position), "*k");
        assert_eq!(key.position.start_line, 4);
        let scalar = &key.children[0];
        assert_eq!(scalar.label, "1");
        assert_eq!(slice(source, &scalar.position), "1");
    }
}

#[test]
fn yaml_scalar_alias_values_anchor_each_reference_occurrence() {
    let old = "name: etl\ntasks: []\nfirst: &a old\nsecond: &b new\nparameters:\n  - name: env\n    default: *a\n";
    let new = old.replace("default: *a", "default: *b");
    let (before, after) = pair(old, &new);
    for (node, source, token, expected) in [
        (&before, old, "*a", "old"),
        (&after, new.as_str(), "*b", "new"),
    ] {
        let parameter = node_by_id(node, "0.param.0").unwrap();
        let property = configuration_property(parameter, "default").unwrap();
        let scalar = &property.children[0];
        assert_eq!(scalar.node_type, "string");
        assert_eq!(scalar.label, expected);
        assert_eq!(slice(source, &scalar.position), token);
    }
}

#[test]
fn complex_multiline_yaml_keys_error_without_rejecting_multiline_values() {
    for key in [
        ">-\n    a\n    b",
        "|-\n    a\n    b",
        "\"a\n    b\"",
        "a\n    b",
    ] {
        let source = format!("name: etl\ntasks: []\nfuture:\n  ? {key}\n  : 1");
        let result: Value = serde_json::from_str(&process_impl(&source)).unwrap();
        let error = result["error"]
            .as_str()
            .unwrap_or_else(|| panic!("{result}"));
        assert!(
            error.contains("Unsupported multiline or block YAML mapping key"),
            "{error}"
        );
    }
    for source in [
        "name: etl\ntasks: []\nfuture:\n  literal: |-\n    a\n    b\n  folded: >-\n    a\n    b\n  quoted: \"a\n    b\"\n",
        "name: etl\ntasks: []\nfuture:\n  plain: a\n    b\n  'a b': 1\n  \"\": 2\n",
    ] {
        let node = tree(source);
        valid(&node, source, &mut HashSet::new());
        assert!(labels(&node).contains(&"a b"));
    }
}

fn node_by_id<'a>(node: &'a SemanticNode, id: &str) -> Option<&'a SemanticNode> {
    if node.id == id {
        return Some(node);
    }
    node.children.iter().find_map(|child| node_by_id(child, id))
}

fn configuration_property<'a>(
    node: &'a SemanticNode,
    name: &str,
) -> Option<&'a SemanticNode> {
    if node.node_type == "property" && node.label == name {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| configuration_property(child, name))
}

#[test]
fn compound_domain_labels_keep_independent_component_identity() {
    for (old, new, id, field, old_value, new_value) in [
        (
            r#"{"name":"etl","tasks":[],"parameters":[{"name":"a : b","default":"c"}]}"#,
            r#"{"name":"etl","tasks":[],"parameters":[{"name":"a","default":"b : c"}]}"#,
            "0.param.0",
            "default",
            "c",
            "b : c",
        ),
        (
            r#"{"name":"etl","tasks":[],"job_clusters":[{"job_cluster_key":"cpu (i3)","new_cluster":{"node_type_id":"large"}}]}"#,
            r#"{"name":"etl","tasks":[],"job_clusters":[{"job_cluster_key":"cpu","new_cluster":{"node_type_id":"i3) (large"}}]}"#,
            "0.cluster.0",
            "node_type_id",
            "large",
            "i3) (large",
        ),
    ] {
        let old_value_tree: Value = serde_json::from_str(old).unwrap();
        let new_value_tree: Value = serde_json::from_str(new).unwrap();
        for (old_source, new_source) in [
            (old.to_owned(), new.to_owned()),
            (
                serde_yaml::to_string(&old_value_tree).unwrap(),
                serde_yaml::to_string(&new_value_tree).unwrap(),
            ),
        ] {
            let (before, after) = pair(&old_source, &new_source);
            let before = node_by_id(&before, id).unwrap();
            let after = node_by_id(&after, id).unwrap();
            // Keep the familiar display label, but never use its ambiguous
            // delimiter concatenation as the only semantic identity.
            assert_eq!(before.label, after.label);
            for (node, source, expected) in [
                (before, old_source.as_str(), old_value),
                (after, new_source.as_str(), new_value),
            ] {
                let property = configuration_property(node, field).unwrap();
                let key: String = serde_yaml::from_str(slice(source, &property.position)).unwrap();
                assert_eq!(key, field);
                let value = &property.children[0];
                assert_eq!(value.node_type, "string");
                assert_eq!(value.label, expected);
                let actual: String = serde_yaml::from_str(slice(source, &value.position)).unwrap();
                assert_eq!(actual, expected);
            }
        }
    }
}

#[test]
fn modeled_defaults_and_cluster_types_keep_typed_value_anchors() {
    for (source, id, field, kind, label, token) in [
        (
            r#"{"name":"etl","tasks":[],"parameters":[{"name":"env","default":"dev"}]}"#,
            "0.param.0", "default", "string", "dev", r#""dev""#,
        ),
        (
            r#"{"name":"etl","tasks":[],"parameters":[{"name":"rate","default":0.100000000000000000001}]}"#,
            "0.param.0", "default", "number", "0.100000000000000000001", "0.100000000000000000001",
        ),
        (
            r#"{"name":"etl","tasks":[],"parameters":[{"name":"switch","default":false}]}"#,
            "0.param.0", "default", "boolean", "false", "false",
        ),
        (
            r#"{"name":"etl","tasks":[],"job_clusters":[{"job_cluster_key":"cpu","new_cluster":{"node_type_id":"i3"}}]}"#,
            "0.cluster.0", "node_type_id", "string", "i3", r#""i3""#,
        ),
    ] {
        let tree = tree(source);
        valid(&tree, source, &mut HashSet::new());
        let domain = node_by_id(&tree, id).unwrap();
        let property = configuration_property(domain, field).unwrap();
        assert_eq!(slice(source, &property.position), format!("\"{field}\""));
        assert_eq!(property.children.len(), 1);
        let value = &property.children[0];
        assert_eq!(value.node_type, kind);
        assert_eq!(value.label, label);
        assert_eq!(slice(source, &value.position), token);
    }
}

fn tree(source: &str) -> SemanticNode {
    let output = process_impl(source);
    serde_json::from_str(&output).unwrap_or_else(|error| panic!("{error}: {output}"))
}
fn labels(node: &SemanticNode) -> Vec<&str> {
    std::iter::once(node.label.as_str())
        .chain(node.children.iter().flat_map(labels))
        .collect()
}
fn valid(node: &SemanticNode, source: &str, ids: &mut HashSet<String>) {
    assert!(ids.insert(node.id.clone()), "duplicate {}", node.id);
    let rows: Vec<_> = source.as_bytes().split(|b| *b == b'\n').collect();
    let p = &node.position;
    assert!((p.start_line, p.start_col) <= (p.end_line, p.end_col));
    for (row, col) in [(p.start_line, p.start_col), (p.end_line, p.end_col)] {
        assert!((row as usize) < rows.len(), "{} row {row}", node.id);
        assert!(
            (col as usize) <= rows[row as usize].len(),
            "{} col {col}",
            node.id
        );
        assert!(std::str::from_utf8(&rows[row as usize][..col as usize]).is_ok());
    }
    for child in &node.children {
        valid(child, source, ids);
    }
}
fn pair(old: &str, new: &str) -> (SemanticNode, SemanticNode) {
    let (a, b) = (tree(old), tree(new));
    valid(&a, old, &mut HashSet::new());
    valid(&b, new, &mut HashSet::new());
    assert_ne!(
        a.structural_hash, b.structural_hash,
        "lost payload: {old} -> {new}"
    );
    (a, b)
}
fn base() -> Value {
    serde_json::json!({
        "name":"etl",
        "tasks":[{
            "task_key":"run",
            "notebook_task":{"notebook_path":"/safe","base_parameters":{"env":"dev"}},
            "libraries":[{"pypi":{"package":"safe==1"}}],
            "timeout_seconds":10,
            "depends_on":[{"task_key":"previous","outcome":"true"}]
        }],
        "job_clusters":[{"job_cluster_key":"cpu","new_cluster":{
            "node_type_id":"Standard","num_workers":1,"spark_version":"15.4"
        }}],
        "schedule":{"quartz_cron_expression":"0 0 1 * * ?","timezone_id":"UTC"}
    })
}
#[test]
fn omitted_execution_fields_retain_values_in_json_and_yaml() {
    let old = base();
    let edits: Vec<(Vec<&str>, Value)> = vec![
        (
            vec!["tasks", "0", "notebook_task", "notebook_path"],
            Value::String("/danger".into()),
        ),
        (
            vec!["tasks", "0", "notebook_task", "base_parameters", "env"],
            Value::String("prod".into()),
        ),
        (
            vec!["tasks", "0", "libraries", "0", "pypi", "package"],
            Value::String("danger==2".into()),
        ),
        (
            vec!["tasks", "0", "timeout_seconds"],
            serde_json::json!(1000),
        ),
        (
            vec!["tasks", "0", "depends_on", "0", "outcome"],
            Value::String("false".into()),
        ),
        (
            vec!["job_clusters", "0", "new_cluster", "num_workers"],
            serde_json::json!(100),
        ),
        (
            vec!["schedule", "quartz_cron_expression"],
            Value::String("0 0 2 * * ?".into()),
        ),
    ];
    for (path, value) in edits {
        let mut changed = old.clone();
        let pointer = format!("/{}", path.join("/"));
        *changed.pointer_mut(&pointer).unwrap() = value.clone();
        for (a, b) in [
            (
                serde_json::to_string(&old).unwrap(),
                serde_json::to_string(&changed).unwrap(),
            ),
            (
                serde_yaml::to_string(&old).unwrap(),
                serde_yaml::to_string(&changed).unwrap(),
            ),
        ] {
            let (_, after) = pair(&a, &b);
            let expected = match &value {
                Value::String(s) => s.clone(),
                _ => value.to_string(),
            };
            assert!(
                labels(&after).contains(&expected.as_str()),
                "missing {pointer}={expected}"
            );
        }
    }
}
#[test]
fn new_unmodeled_nested_fields_and_scalar_kinds_are_preserved() {
    for (a, b) in [
        (
            r#"{"name":"etl","tasks":[],"future":{"switch":true}}"#,
            r#"{"name":"etl","tasks":[],"future":{"switch":"true"}}"#,
        ),
        (
            r#"{"name":"etl","tasks":[],"future":{"x":null}}"#,
            r#"{"name":"etl","tasks":[],"future":{"x":{}}}"#,
        ),
        (
            r#"{"name":"etl","tasks":[],"future":{"a.b":{"a/b":1}}}"#,
            r#"{"name":"etl","tasks":[],"future":{"a.b":{"a/b":2}}}"#,
        ),
        (
            r#"{"name":"etl","tasks":[],"future":{"x":"λ"}}"#,
            r#"{"name":"etl","tasks":[],"future":{"x":"μ"}}"#,
        ),
        (
            r#"{"name":"etl","tasks":[]}"#,
            r#"{"name":"etl","tasks":[],"future":null}"#,
        ),
        (r#"{"name":"job","tasks":[]}"#, r#"{"tasks":[]}"#),
        (
            r#"{"name":"a b","tasks":[]}"#,
            r#"{"name":"a  b","tasks":[]}"#,
        ),
        (
            r#"{"name":"etl","parameters":[{"name":"env","default":"a b"}],"tasks":[]}"#,
            r#"{"name":"etl","parameters":[{"name":"env","default":"a  b"}],"tasks":[]}"#,
        ),
        (r#"{"name":"etl","tasks":[]}"#, r#"{"name":"etl"}"#),
    ] {
        pair(a, b);
    }
}
#[test]
fn configuration_numbers_keep_exact_source_identity() {
    for (old, new) in [
        (
            r#"{"name":"etl","tasks":[],"threshold":0.100000000000000000001}"#,
            r#"{"name":"etl","tasks":[],"threshold":0.100000000000000000002}"#,
        ),
        (
            "name: etl\ntasks: []\nthreshold: 0.100000000000000000001\n",
            "name: etl\ntasks: []\nthreshold: 0.100000000000000000002\n",
        ),
        (
            r#"{"name":"etl","parameters":[{"name":"threshold","default":0.100000000000000000001}],"tasks":[]}"#,
            r#"{"name":"etl","parameters":[{"name":"threshold","default":0.100000000000000000002}],"tasks":[]}"#,
        ),
    ] {
        pair(old, new);
    }
}
#[test]
fn all_array_item_order_and_empty_container_presence_survive() {
    for (old, new) in [
        (
            r#"{"name":"etl","tasks":[{"task_key":"a","libraries":["dbfs:/a"],"depends_on":["previous"]}]}"#,
            r#"{"name":"etl","tasks":[{"task_key":"a","libraries":[true],"depends_on":["previous"]}]}"#,
        ),
        (
            r#"{"name":"etl","tasks":[],"future":["a","b"]}"#,
            r#"{"name":"etl","tasks":[],"future":["b","a"]}"#,
        ),
        (
            r#"{"name":"etl","tasks":[{"task_key":"a","notebook_task":{}},null]}"#,
            r#"{"name":"etl","tasks":[null,{"task_key":"a","notebook_task":{}}]}"#,
        ),
        (
            r#"{"name":"etl","parameters":[{"name":"x","default":"a"},{}],"tasks":[]}"#,
            r#"{"name":"etl","parameters":[{},{"name":"x","default":"a"}],"tasks":[]}"#,
        ),
    ] {
        pair(old, new);
    }
}
#[test]
fn complete_configuration_keeps_identity_map_order_and_layout_controls() {
    let json = serde_json::to_string(&base()).unwrap();
    let pretty = serde_json::to_string_pretty(&base()).unwrap();
    assert_eq!(tree(&json).structural_hash, tree(&pretty).structural_hash);
    for (old,new) in [
        ("name: etl\ntasks: []\nfuture:\n  amount: 12\n  text: 'quoted # text'\n","future:\n  text: \"quoted # text\" # trailing\n  amount: 12 # cents\ntasks: []\nname: etl\n"),
        (r#"{"name":"etl","tasks":[],"future":{"b":2,"a":1}}"#,r#"{"future":{"a":1,"b":2},"tasks":[],"name":"etl"}"#),
    ] {
        let (a,b)=(tree(old),tree(new));
        valid(&a,old,&mut HashSet::new());valid(&b,new,&mut HashSet::new());
        assert_eq!(a.structural_hash,b.structural_hash,"layout became payload");
    }
    assert_eq!(process_impl(&json), process_impl(&json));
}
