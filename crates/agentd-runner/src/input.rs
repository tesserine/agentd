use crate::types::{InvocationInput, RunnerError};
use jsonschema::validator_for;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const INVOCATION_INPUT_MOUNT_PATH: &str = "/agentd/invocation-input";
const REQUEST_ARTIFACT_TYPE: &str = "request";
const REQUEST_ARTIFACT_ID: &str = "operator-input";
const REQUEST_SOURCE: &str = "operator";
const SUPPORTED_REQUEST_CANONICAL_VERSIONS: &[&str] = &["1.0.0"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedInvocationInput {
    pub(crate) artifact_type: String,
    pub(crate) artifact_id: String,
    pub(crate) document_json: String,
}

#[derive(Debug, Default, Deserialize)]
struct MethodologyManifest {
    #[serde(default)]
    artifact_types: Vec<DeclaredArtifactType>,
}

#[derive(Debug, Deserialize)]
struct DeclaredArtifactType {
    name: String,
}

pub(crate) fn resolve_invocation_input(
    methodology_dir: &Path,
    input: Option<&InvocationInput>,
) -> Result<Option<ResolvedInvocationInput>, RunnerError> {
    let Some(input) = input else {
        return Ok(None);
    };

    let manifest = load_manifest(methodology_dir)?;
    match input {
        InvocationInput::RequestText { description } => {
            let schema_path = methodology_dir.join("schemas/request.schema.json");
            ensure_declared_artifact_type(&manifest, REQUEST_ARTIFACT_TYPE).map_err(|message| {
                RunnerError::InvalidInvocationInput {
                    message: format!(
                        "methodology does not support operator request input: {message}"
                    ),
                }
            })?;
            let schema = load_schema(&schema_path).map_err(|message| {
                RunnerError::InvalidInvocationInput {
                    message: format!(
                        "methodology does not support operator request input: {message}"
                    ),
                }
            })?;
            ensure_supported_request_version(&schema).map_err(|message| {
                RunnerError::InvalidInvocationInput {
                    message: format!(
                        "methodology does not support operator request input: {message}"
                    ),
                }
            })?;

            let document = json!({
                "description": description,
                "source": REQUEST_SOURCE,
            });
            validate_document(&schema, &document, REQUEST_ARTIFACT_TYPE)?;

            Ok(Some(ResolvedInvocationInput {
                artifact_type: REQUEST_ARTIFACT_TYPE.to_string(),
                artifact_id: REQUEST_ARTIFACT_ID.to_string(),
                document_json: render_document(&document)?,
            }))
        }
        InvocationInput::Artifact {
            artifact_type,
            artifact_id,
            document,
        } => {
            ensure_declared_artifact_type(&manifest, artifact_type)
                .map_err(|message| RunnerError::InvalidInvocationInput { message })?;
            if artifact_id.is_empty() {
                return Err(RunnerError::InvalidInvocationInput {
                    message: format!(
                        "artifact input for type '{artifact_type}' must include a non-empty artifact id"
                    ),
                });
            }

            let schema = load_schema(&schema_path(methodology_dir, artifact_type))
                .map_err(|message| RunnerError::InvalidInvocationInput { message })?;
            validate_document(&schema, document, artifact_type)?;

            Ok(Some(ResolvedInvocationInput {
                artifact_type: artifact_type.clone(),
                artifact_id: artifact_id.clone(),
                document_json: render_document(document)?,
            }))
        }
    }
}

fn load_manifest(methodology_dir: &Path) -> Result<MethodologyManifest, RunnerError> {
    let manifest_path = methodology_dir.join("manifest.toml");
    let manifest = fs::read_to_string(&manifest_path).map_err(RunnerError::Io)?;
    toml::from_str(&manifest).map_err(|error| RunnerError::InvalidInvocationInput {
        message: format!(
            "failed to parse methodology manifest {}: {error}",
            manifest_path.display()
        ),
    })
}

fn ensure_declared_artifact_type(
    manifest: &MethodologyManifest,
    artifact_type: &str,
) -> Result<(), String> {
    if manifest
        .artifact_types
        .iter()
        .any(|declared| declared.name == artifact_type)
    {
        return Ok(());
    }

    Err(format!(
        "artifact type '{artifact_type}' is not declared in the methodology manifest"
    ))
}

fn schema_path(methodology_dir: &Path, artifact_type: &str) -> PathBuf {
    methodology_dir
        .join("schemas")
        .join(format!("{artifact_type}.schema.json"))
}

fn load_schema(path: &Path) -> Result<Value, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("failed to read schema {}: {error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("schema {} must contain valid JSON: {error}", path.display()))
}

fn ensure_supported_request_version(schema: &Value) -> Result<(), String> {
    let version = schema
        .get("x-tesserine-canonical")
        .and_then(|value| value.get("version"))
        .and_then(Value::as_str)
        .ok_or_else(|| "request schema must declare x-tesserine-canonical.version".to_string())?;
    if SUPPORTED_REQUEST_CANONICAL_VERSIONS.contains(&version) {
        return Ok(());
    }

    Err(format!(
        "canonical request version {version} is not supported"
    ))
}

fn validate_document(
    schema: &Value,
    document: &Value,
    artifact_type: &str,
) -> Result<(), RunnerError> {
    let validator = validator_for(schema).map_err(|error| RunnerError::InvalidInvocationInput {
        message: format!("schema for artifact type '{artifact_type}' is invalid: {error}"),
    })?;
    let mut errors = validator.iter_errors(document);
    if let Some(error) = errors.next() {
        return Err(RunnerError::InvalidInvocationInput {
            message: format!(
                "artifact input for type '{artifact_type}' does not match the methodology schema: {error}"
            ),
        });
    }

    Ok(())
}

fn render_document(document: &Value) -> Result<String, RunnerError> {
    serde_json::to_string(document).map_err(|error| RunnerError::InvalidInvocationInput {
        message: format!("failed to serialize invocation input document: {error}"),
    })
}
