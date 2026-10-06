use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::{Access, Tool};

const MAX_REF_DEPTH: usize = 64;
const MAX_OPERATIONS: usize = super::MAX_CATALOG_TOOLS;

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub location: String,
    pub explode: bool,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Operation {
    pub method: String,
    pub path: String,
    pub parameters: Vec<Parameter>,
    pub unsupported: Option<String>,
}

fn resolve(value: &Value, root: &Value, depth: usize) -> Result<Value> {
    if depth > MAX_REF_DEPTH {
        bail!("Recursive or excessively nested OpenAPI reference");
    }
    if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        let pointer = reference
            .strip_prefix('#')
            .context("External OpenAPI references must be bundled before import")?;
        return resolve(
            root.pointer(pointer)
                .context("Unresolved OpenAPI reference")?,
            root,
            depth + 1,
        );
    }
    Ok(value.clone())
}

fn schema_references(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                output.push(reference.into());
            }
            for value in object.values() {
                schema_references(value, output);
            }
        }
        Value::Array(values) => {
            for value in values {
                schema_references(value, output);
            }
        }
        _ => {}
    }
}

fn with_definitions(mut schema: Value, root: &Value) -> Result<Value> {
    let mut references = Vec::new();
    schema_references(&schema, &mut references);
    let mut definitions = Map::new();
    while let Some(reference) = references.pop() {
        let name = reference
            .strip_prefix("#/components/schemas/")
            .context("Only bundled local component schema references are supported")?
            .split('/')
            .next()
            .context("Invalid schema reference")?
            .replace("~1", "/")
            .replace("~0", "~");
        if definitions.contains_key(&name) {
            continue;
        }
        let value = root["components"]["schemas"]
            .get(&name)
            .context("Unresolved component schema")?
            .clone();
        schema_references(&value, &mut references);
        definitions.insert(name, value);
        if definitions.len() > MAX_OPERATIONS {
            bail!("Too many component schema references");
        }
    }
    if !definitions.is_empty() {
        schema["components"] = json!({"schemas":definitions});
    }
    Ok(schema)
}

fn normalize_schema(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                normalize_schema(value);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                normalize_schema(value);
            }
            for (exclusive, bound) in [
                ("exclusiveMinimum", "minimum"),
                ("exclusiveMaximum", "maximum"),
            ] {
                if let Some(enabled) = object.get(exclusive).and_then(Value::as_bool) {
                    object.remove(exclusive);
                    if enabled {
                        if let Some(value) = object.remove(bound) {
                            object.insert(exclusive.into(), value);
                        }
                    }
                }
            }
            if object.remove("nullable").and_then(|value| value.as_bool()) == Some(true) {
                let non_null = Value::Object(std::mem::take(object));
                object.insert("anyOf".into(), json!([non_null, {"type":"null"}]));
            }
        }
        _ => {}
    }
}

pub(super) fn import(root: &Value) -> Result<Vec<Tool>> {
    let version = root["openapi"]
        .as_str()
        .context("An OpenAPI 3.0 or 3.1 JSON document is required")?;
    if !version.starts_with("3.0.") && !version.starts_with("3.1.") {
        bail!("Supported OpenAPI versions are 3.0 and 3.1");
    }
    let paths = root["paths"]
        .as_object()
        .context("OpenAPI paths are required")?;
    let mut tools = Vec::new();
    let mut names = std::collections::HashSet::new();
    let mut catalog_bytes = 0usize;
    for (path, item) in paths {
        if !path.starts_with('/')
            || path.starts_with("//")
            || path.contains('?')
            || path.contains('#')
        {
            bail!("Invalid OpenAPI operation path");
        }
        let item = resolve(item, root, 0)?;
        let Some(methods) = item.as_object() else {
            bail!("Invalid OpenAPI path item");
        };
        for (method, definition) in methods {
            if !matches!(
                method.as_str(),
                "get" | "post" | "put" | "patch" | "delete" | "head" | "options"
            ) {
                continue;
            }
            let name = definition["operationId"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{}_{}", method, path));
            if !names.insert(name.clone()) {
                bail!("Duplicate OpenAPI operation ID: {name}");
            }
            let mut properties = Map::new();
            let mut required_groups = Vec::new();
            let mut parameters = Vec::new();
            let mut unsupported = None;
            let mut grouped =
                std::collections::BTreeMap::<String, (Map<String, Value>, Vec<String>)>::new();
            let all = item["parameters"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(definition["parameters"].as_array().into_iter().flatten());
            for parameter in all {
                let parameter = resolve(parameter, root, 0)?;
                let pname = parameter["name"]
                    .as_str()
                    .context("Parameter name missing")?;
                let location = parameter["in"]
                    .as_str()
                    .context("Parameter location missing")?;
                let group = match location {
                    "path" => "path",
                    "query" => "query",
                    "header" => "headers",
                    _ => {
                        unsupported = Some("Cookie parameters are not supported".into());
                        continue;
                    }
                };
                let schema = parameter
                    .get("schema")
                    .cloned()
                    .unwrap_or(json!({"type":"string"}));
                let default_style = if location == "query" {
                    "form"
                } else {
                    "simple"
                };
                if parameter["style"].as_str().unwrap_or(default_style) != default_style
                    || schema["type"] == "object"
                {
                    unsupported = Some("This parameter serialization is not supported".into());
                }
                if location == "header"
                    && matches!(
                        pname.to_ascii_lowercase().as_str(),
                        "authorization" | "cookie" | "host" | "proxy-authorization"
                    )
                {
                    unsupported =
                        Some("Authentication headers must be configured as credentials".into());
                }
                let group_entry = grouped.entry(group.into()).or_default();
                group_entry.0.insert(pname.into(), schema);
                group_entry.1.retain(|name| name != pname);
                if parameter["required"].as_bool().unwrap_or(false) || location == "path" {
                    group_entry.1.push(pname.into());
                }
                parameters.retain(|p: &Parameter| p.name != pname || p.location != location);
                parameters.push(Parameter {
                    name: pname.into(),
                    location: location.into(),
                    explode: parameter["explode"]
                        .as_bool()
                        .unwrap_or(location == "query"),
                });
            }
            for (group, (props, required)) in grouped {
                if !required.is_empty() {
                    required_groups.push(group.clone());
                }
                properties.insert(group,json!({"type":"object","properties":props,"required":required,"additionalProperties":false}));
            }
            if let Some(body) = definition.get("requestBody") {
                let body = resolve(body, root, 0)?;
                if let Some(schema) = body.pointer("/content/application~1json/schema") {
                    properties.insert("body".into(), schema.clone());
                    if body["required"].as_bool().unwrap_or(false) {
                        required_groups.push("body".into());
                    }
                } else {
                    unsupported = Some("Only JSON request bodies are supported".into());
                }
            }
            let mut input_schema = with_definitions(
                json!({"type":"object","properties":properties,"required":required_groups,"additionalProperties":false}),
                root,
            )?;
            if version.starts_with("3.0.") {
                normalize_schema(&mut input_schema);
            }
            catalog_bytes = catalog_bytes.saturating_add(serde_json::to_vec(&input_schema)?.len());
            if catalog_bytes > super::MAX_CATALOG_BYTES {
                bail!("Expanded OpenAPI catalog exceeds the supported limit");
            }
            tools.push(Tool {
                name,
                description: definition["description"]
                    .as_str()
                    .or_else(|| definition["summary"].as_str())
                    .unwrap_or("")
                    .into(),
                input_schema,
                suggested_access: if matches!(method.as_str(), "get" | "head" | "options") {
                    Access::Read
                } else {
                    Access::Write
                },
                operation: Some(Operation {
                    method: method.to_uppercase(),
                    path: path.into(),
                    parameters,
                    unsupported,
                }),
            });
            if tools.len() > MAX_OPERATIONS {
                bail!("OpenAPI document has too many operations");
            }
        }
    }
    if tools.is_empty() {
        bail!("OpenAPI document contains no operations");
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_local_references_and_required_arguments() {
        let tools = import(&json!({"openapi":"3.1.0","components":{"schemas":{"Order":{"type":"object","required":["name"],"properties":{"name":{"type":"string"}}}}},"paths":{"/orders/{id}":{"parameters":[{"in":"path","name":"id","required":true,"schema":{"type":"integer"}}],"patch":{"operationId":"update_order","requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Order"}}}}}}}})).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].suggested_access, Access::Write);
        assert_eq!(
            tools[0].input_schema["properties"]["body"]["$ref"],
            json!("#/components/schemas/Order")
        );
        assert!(super::super::transport::validate_arguments(
            &tools[0],
            &json!({"path":{"id":1},"body":{"name":"order"}})
        )
        .is_ok());
        assert!(super::super::transport::validate_arguments(
            &tools[0],
            &json!({"path":{"id":1},"body":{}})
        )
        .is_err());
        assert_eq!(tools[0].input_schema["required"], json!(["path", "body"]));
    }

    #[test]
    fn honors_nullable_and_exclusive_bounds_in_openapi_30() {
        let tools = import(&json!({"openapi":"3.0.3","paths":{"/items":{"post":{"requestBody":{"required":true,"content":{"application/json":{"schema":{"type":"number","nullable":true,"minimum":1,"exclusiveMinimum":true}}}}}}}})).unwrap();
        let validate =
            |body| super::super::transport::validate_arguments(&tools[0], &json!({"body":body}));
        assert!(validate(Value::Null).is_ok());
        assert!(validate(json!(2)).is_ok());
        assert!(validate(json!(1)).is_err());
    }

    #[test]
    fn operation_parameters_override_path_parameters() {
        let tools = import(&json!({"openapi":"3.1.0","paths":{"/items":{"parameters":[{"name":"q","in":"query","required":true,"schema":{"type":"string"}}],"get":{"parameters":[{"name":"q","in":"query","required":false,"schema":{"type":"integer"}}]}}}})).unwrap();
        assert!(super::super::transport::validate_arguments(&tools[0], &json!({})).is_ok());
        assert!(
            super::super::transport::validate_arguments(&tools[0], &json!({"query":{"q":7}}))
                .is_ok()
        );
        assert!(super::super::transport::validate_arguments(
            &tools[0],
            &json!({"query":{"q":"wrong"}})
        )
        .is_err());
    }

    #[test]
    fn rejects_external_references_and_duplicate_ids() {
        assert!(resolve(&json!({"$ref":"other.json"}), &json!({}), 0).is_err());
        assert!(import(&json!({"openapi":"3.0.3","paths":{"/one":{"get":{"operationId":"duplicate"}},"/two":{"get":{"operationId":"duplicate"}}}})).is_err());
    }
}
