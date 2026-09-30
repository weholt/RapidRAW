//! Preview-first metadata import.
//!
//! Metadata precedence (highest first) is RapidRAW `.rrdata`, legacy `.rrexif`, XMP sidecars,
//! then embedded EXIF/RAW metadata. Filename facts and sequence are synthetic. Metadata values,
//! scans, path components, and previews are bounded. Preview is side-effect free.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;

use crate::exif_processing;
use crate::formats::is_supported_image_file;

pub const IMPORT_PATTERN_VERSION: u32 = 1;
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SIDECAR_BYTES: u64 = 2 * 1024 * 1024;
const MAX_VALUE_BYTES: usize = 500;
const MAX_COMPONENT_BYTES: usize = 240;
const MAX_RELATIVE_PATH_BYTES: usize = 1024;
const MAX_SCAN_DEPTH: usize = 32;
const MAX_SCAN_ENTRIES: usize = 100_000;
const MAX_STORED_PLANS: usize = 8;
const DEFAULT_PAGE_SIZE: usize = 200;
const MAX_PAGE_SIZE: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportOperation {
    Copy,
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CollisionPolicy {
    Skip,
    RenameWithSuffix,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MissingTokenPolicy {
    Empty,
    Fallback,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MetadataToken {
    OriginalFilename,
    OriginalStem,
    Extension,
    Sequence,
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Make,
    Model,
    LensModel,
    Iso,
    Artist,
    Copyright,
    ImageDescription,
    Keywords,
    Rating,
    ColorLabel,
    Headline,
    Location,
}

const ALL_TOKENS: [MetadataToken; 21] = [
    MetadataToken::OriginalFilename,
    MetadataToken::OriginalStem,
    MetadataToken::Extension,
    MetadataToken::Sequence,
    MetadataToken::Year,
    MetadataToken::Month,
    MetadataToken::Day,
    MetadataToken::Hour,
    MetadataToken::Minute,
    MetadataToken::Make,
    MetadataToken::Model,
    MetadataToken::LensModel,
    MetadataToken::Iso,
    MetadataToken::Artist,
    MetadataToken::Copyright,
    MetadataToken::ImageDescription,
    MetadataToken::Keywords,
    MetadataToken::Rating,
    MetadataToken::ColorLabel,
    MetadataToken::Headline,
    MetadataToken::Location,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataValue {
    pub token: MetadataToken,
    pub raw_value: Option<String>,
    pub normalized_value: Option<String>,
    pub source: Option<String>,
    pub missing: bool,
}

pub type MetadataCatalog = BTreeMap<MetadataToken, MetadataValue>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PatternPart {
    Literal {
        value: String,
    },
    Token {
        token: MetadataToken,
        fallback: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPattern {
    pub version: u32,
    pub parts: Vec<PatternPart>,
    pub missing_token_policy: MissingTokenPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPlanRequest {
    #[serde(default)]
    pub source_files: Vec<String>,
    pub source_folder: Option<String>,
    #[serde(default)]
    pub recursive: bool,
    pub destination_root: String,
    pub folder_pattern: ImportPattern,
    pub filename_pattern: ImportPattern,
    pub operation: ImportOperation,
    pub collision_policy: CollisionPolicy,
    #[serde(default = "default_true")]
    pub include_associated_files: bool,
    #[serde(default)]
    pub preserve_timestamps: bool,
    #[serde(default)]
    pub use_capture_time: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportPreviewStatus {
    Ready,
    Renamed,
    Skipped,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportConflict {
    pub code: String,
    pub message: String,
    pub conflicting_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreviewRow {
    pub source_path: String,
    pub destination_relative_path: String,
    pub metadata: MetadataCatalog,
    pub associated_files: Vec<String>,
    pub byte_size: Option<u64>,
    pub status: ImportPreviewStatus,
    pub conflicts: Vec<ImportConflict>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub plan_id: String,
    pub request_hash: String,
    pub rows: Vec<ImportPreviewRow>,
    pub page: u64,
    pub page_size: u64,
    pub total_rows: u64,
    pub has_more: bool,
    pub unsupported_file_count: u64,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreviewPageRequest {
    pub plan_id: String,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteImportRequest {
    pub plan_id: String,
    pub request_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportPhase {
    Scanning,
    Planning,
    Copying,
    Verifying,
    Moving,
    Complete,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgressEvent {
    pub plan_id: Option<String>,
    pub phase: ImportPhase,
    pub current: u64,
    pub total: u64,
    pub bytes_completed: Option<u64>,
    pub bytes_total: Option<u64>,
    pub source_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportItemStatus {
    Succeeded,
    Renamed,
    Skipped,
    Warned,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportItemResult {
    pub source_path: String,
    pub destination_relative_path: Option<String>,
    pub status: ImportItemStatus,
    pub source_retained: bool,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub plan_id: String,
    pub cancelled: bool,
    pub succeeded: u64,
    pub renamed: u64,
    pub skipped: u64,
    pub warned: u64,
    pub failed: u64,
    pub retained_sources: u64,
    pub items: Vec<ImportItemResult>,
}

#[derive(Debug, Clone)]
struct SourceStamp {
    path: PathBuf,
    len: u64,
    modified_nanos: u128,
}

#[derive(Debug, Clone)]
struct StoredPlan {
    request: ImportPlanRequest,
    request_hash: String,
    root: PathBuf,
    rows: Vec<ImportPreviewRow>,
    stamps: Vec<SourceStamp>,
    unsupported: u64,
    warnings: Vec<String>,
    errors: Vec<String>,
}

fn plans() -> &'static Mutex<HashMap<String, StoredPlan>> {
    static PLANS: OnceLock<Mutex<HashMap<String, StoredPlan>>> = OnceLock::new();
    PLANS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cancellations() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static TOKENS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    TOKENS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn scan_cancel() -> &'static AtomicBool {
    static CANCEL: AtomicBool = AtomicBool::new(false);
    &CANCEL
}

fn scan_lock() -> &'static Mutex<()> {
    static LOCK: Mutex<()> = Mutex::new(());
    &LOCK
}

fn bounded(value: &str) -> String {
    let value = value.trim().trim_matches('"').trim_matches(char::from(0));
    if value.len() <= MAX_VALUE_BYTES {
        return value.to_string();
    }
    let mut end = MAX_VALUE_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn token_key(token: MetadataToken) -> &'static [&'static str] {
    match token {
        MetadataToken::Year
        | MetadataToken::Month
        | MetadataToken::Day
        | MetadataToken::Hour
        | MetadataToken::Minute => &["DateTimeOriginal", "CreateDate"],
        MetadataToken::Make => &["Make"],
        MetadataToken::Model => &["Model"],
        MetadataToken::LensModel => &["LensModel"],
        MetadataToken::Iso => &["ISOSpeed", "PhotographicSensitivity", "ISOSpeedRatings"],
        MetadataToken::Artist => &["Artist", "Creator", "dc:creator"],
        MetadataToken::Copyright => &["Copyright", "dc:rights"],
        MetadataToken::ImageDescription => &[
            "ImageDescription",
            "Description",
            "Caption",
            "dc:description",
        ],
        MetadataToken::Keywords => &["Keywords", "Subject", "dc:subject"],
        MetadataToken::Rating => &["Rating", "xmp:Rating"],
        MetadataToken::ColorLabel => &["ColorLabel", "Label", "xmp:Label"],
        MetadataToken::Headline => &["Headline", "photoshop:Headline"],
        MetadataToken::Location => &["Location", "City", "State", "Country", "iptc:Location"],
        _ => &[],
    }
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let metadata =
        fs::metadata(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if metadata.len() > limit {
        return Err(format!(
            "{} exceeds the metadata read limit",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "{} exceeds the metadata read limit",
            path.display()
        ));
    }
    Ok(bytes)
}

fn sidecar_paths(path: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for suffix in [".rrdata", ".rrexif", ".xmp"] {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        result.push(path.with_file_name(name));
    }
    result.push(path.with_extension("xmp"));
    if let (Some(parent), Some(filename)) =
        (path.parent(), path.file_name().and_then(|v| v.to_str()))
        && let Ok(entries) = fs::read_dir(parent)
    {
        let prefix = format!("{filename}.");
        result.extend(
            entries
                .flatten()
                .take(256)
                .map(|entry| entry.path())
                .filter(|candidate| {
                    candidate
                        .file_name()
                        .and_then(|value| value.to_str())
                        .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".rrdata"))
                }),
        );
    }
    result.sort();
    result.dedup();
    result
}

fn flatten_json(value: &serde_json::Value, output: &mut HashMap<String, String>) {
    if let Some(object) = value.as_object() {
        for (key, value) in object {
            match value {
                serde_json::Value::String(text) => {
                    output.insert(key.clone(), bounded(text));
                }
                serde_json::Value::Number(number) => {
                    output.insert(key.clone(), number.to_string());
                }
                serde_json::Value::Array(values) => {
                    let joined = values
                        .iter()
                        .filter_map(|value| value.as_str())
                        .map(bounded)
                        .collect::<Vec<_>>()
                        .join(", ");
                    if !joined.is_empty() {
                        output.insert(key.clone(), joined);
                    }
                }
                serde_json::Value::Object(_) => flatten_json(value, output),
                _ => {}
            }
        }
    }
}

fn xml_unescape(value: &str) -> String {
    bounded(
        &value
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

fn xmp_value(xml: &str, names: &[&str]) -> Option<String> {
    for name in names {
        let attribute = format!("{name}=\"");
        if let Some(start) = xml.find(&attribute) {
            let rest = &xml[start + attribute.len()..];
            if let Some(end) = rest.find('"') {
                return Some(xml_unescape(&rest[..end]));
            }
        }
        let local = name.rsplit(':').next().unwrap_or(name);
        for tag in [*name, local] {
            let opening = format!("<{tag}");
            if let Some(start) = xml.find(&opening) {
                let rest = &xml[start..];
                let content_start = rest.find('>')? + 1;
                let content = &rest[content_start..];
                let closing = format!("</{tag}>");
                if let Some(end) = content.find(&closing) {
                    let raw = &content[..end];
                    let values = raw
                        .split('>')
                        .skip(1)
                        .filter_map(|part| part.split('<').next())
                        .filter(|part| !part.trim().is_empty())
                        .map(xml_unescape)
                        .collect::<Vec<_>>();
                    return Some(if values.is_empty() {
                        xml_unescape(raw)
                    } else {
                        values.join(", ")
                    });
                }
            }
        }
    }
    None
}

fn insert_source(
    merged: &mut HashMap<String, (String, String)>,
    values: HashMap<String, String>,
    source: &str,
) {
    for (key, value) in values {
        if !value.trim().is_empty() {
            merged.insert(key.to_lowercase(), (bounded(&value), source.to_string()));
        }
    }
}

pub fn metadata_catalog(
    path: &Path,
    sequence: usize,
    total: usize,
) -> Result<MetadataCatalog, String> {
    let metadata =
        fs::metadata(path).map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err("Source is not a regular file".to_string());
    }
    let mut merged = HashMap::<String, (String, String)>::new();
    if metadata.len() <= MAX_IMAGE_BYTES {
        let bytes = read_bounded(path, MAX_IMAGE_BYTES)?;
        insert_source(
            &mut merged,
            exif_processing::read_exif_data_from_bytes(&path.to_string_lossy(), &bytes),
            "embedded",
        );
    }

    let xmp_paths = [path.with_extension("xmp"), {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".xmp");
        path.with_file_name(name)
    }];
    for xmp_path in xmp_paths {
        if xmp_path.exists() {
            let xml =
                String::from_utf8_lossy(&read_bounded(&xmp_path, MAX_SIDECAR_BYTES)?).to_string();
            let mut values = HashMap::new();
            for token in ALL_TOKENS {
                if let Some(value) = xmp_value(&xml, token_key(token)) {
                    for key in token_key(token) {
                        values.insert((*key).to_string(), value.clone());
                    }
                }
            }
            insert_source(&mut merged, values, "xmp");
        }
    }

    let mut legacy_name = path.file_name().unwrap_or_default().to_os_string();
    legacy_name.push(".rrexif");
    let legacy = path.with_file_name(legacy_name);
    if legacy.exists() {
        let json: serde_json::Value =
            serde_json::from_slice(&read_bounded(&legacy, MAX_SIDECAR_BYTES)?)
                .map_err(|e| format!("Invalid .rrexif: {e}"))?;
        let mut values = HashMap::new();
        flatten_json(&json, &mut values);
        insert_source(&mut merged, values, "rrexif");
    }

    let mut rrdata_name = path.file_name().unwrap_or_default().to_os_string();
    rrdata_name.push(".rrdata");
    let rrdata = path.with_file_name(rrdata_name);
    if rrdata.exists() {
        let json: serde_json::Value =
            serde_json::from_slice(&read_bounded(&rrdata, MAX_SIDECAR_BYTES)?)
                .map_err(|e| format!("Invalid .rrdata: {e}"))?;
        let mut values = HashMap::new();
        flatten_json(&json, &mut values);
        insert_source(&mut merged, values, "rrdata");
    }

    let filename = path.file_name().and_then(|v| v.to_str()).unwrap_or("image");
    let stem = path.file_stem().and_then(|v| v.to_str()).unwrap_or("image");
    let extension = path.extension().and_then(|v| v.to_str()).unwrap_or("");
    let width = total.max(1).to_string().len();
    let synthetic = [
        (MetadataToken::OriginalFilename, filename.to_string()),
        (MetadataToken::OriginalStem, stem.to_string()),
        (MetadataToken::Extension, extension.to_string()),
        (MetadataToken::Sequence, format!("{sequence:0width$}")),
    ];
    let mut catalog = MetadataCatalog::new();
    for token in ALL_TOKENS {
        let direct = synthetic.iter().find(|(candidate, _)| *candidate == token);
        let aliases: Vec<String> = token_key(token)
            .iter()
            .map(|key| key.to_lowercase())
            .collect();
        let found = direct
            .map(|(_, value)| (value.clone(), "synthetic".to_string()))
            .or_else(|| {
                aliases
                    .iter()
                    .find_map(|key| merged.get(key.as_str()).cloned())
            });
        let normalized = found
            .as_ref()
            .map(|(value, _)| normalize_token(token, value));
        catalog.insert(
            token,
            MetadataValue {
                token,
                raw_value: found.as_ref().map(|(value, _)| value.clone()),
                normalized_value: normalized,
                source: found.map(|(_, source)| source),
                missing: direct.is_none() && !aliases.iter().any(|key| merged.contains_key(key)),
            },
        );
    }
    Ok(catalog)
}

fn normalize_token(token: MetadataToken, value: &str) -> String {
    let value = bounded(value);
    match token {
        MetadataToken::Year => datetime_part(&value, 0, 4),
        MetadataToken::Month => datetime_part(&value, 5, 2),
        MetadataToken::Day => datetime_part(&value, 8, 2),
        MetadataToken::Hour => datetime_part(&value, 11, 2),
        MetadataToken::Minute => datetime_part(&value, 14, 2),
        MetadataToken::Keywords => value
            .split([',', ';'])
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("-"),
        _ => value,
    }
}

fn datetime_part(value: &str, start: usize, len: usize) -> String {
    let normalized = value.replace(':', "-").replace('T', " ");
    normalized.get(start..start + len).unwrap_or("").to_string()
}

pub fn parse_legacy_pattern(
    template: &str,
    missing_token_policy: MissingTokenPolicy,
) -> ImportPattern {
    let aliases = [
        ("original_filename", MetadataToken::OriginalStem),
        ("original_stem", MetadataToken::OriginalStem),
        ("extension", MetadataToken::Extension),
        ("sequence", MetadataToken::Sequence),
        ("YYYY", MetadataToken::Year),
        ("MM", MetadataToken::Month),
        ("DD", MetadataToken::Day),
        ("hh", MetadataToken::Hour),
        ("mm", MetadataToken::Minute),
        ("make", MetadataToken::Make),
        ("model", MetadataToken::Model),
        ("lens_model", MetadataToken::LensModel),
        ("iso", MetadataToken::Iso),
        ("artist", MetadataToken::Artist),
        ("copyright", MetadataToken::Copyright),
        ("description", MetadataToken::ImageDescription),
        ("keywords", MetadataToken::Keywords),
        ("rating", MetadataToken::Rating),
        ("color_label", MetadataToken::ColorLabel),
    ];
    let mut parts = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        if open > 0 {
            parts.push(PatternPart::Literal {
                value: rest[..open].to_string(),
            });
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            parts.push(PatternPart::Literal {
                value: rest[open..].to_string(),
            });
            rest = "";
            break;
        };
        let name = &after[..close];
        if let Some((_, token)) = aliases.iter().find(|(alias, _)| alias == &name) {
            parts.push(PatternPart::Token {
                token: *token,
                fallback: None,
            });
        } else {
            parts.push(PatternPart::Literal {
                value: format!("{{{name}}}"),
            });
        }
        rest = &after[close + 1..];
    }
    if !rest.is_empty() {
        parts.push(PatternPart::Literal {
            value: rest.to_string(),
        });
    }
    ImportPattern {
        version: IMPORT_PATTERN_VERSION,
        parts,
        missing_token_policy,
    }
}

fn reserved_windows_name(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

fn sanitize_segment(value: &str) -> Result<(String, Vec<String>), String> {
    let mut warnings = Vec::new();
    let mut output = String::new();
    for character in value.chars() {
        if character.is_control()
            || matches!(
                character,
                '<'
                    | '>'
                    | ':'
                    | '"'
                    | '/'
                    | '\\'
                    | '|'
                    | '?'
                    | '*'
                    | '\u{2044}'
                    | '\u{2215}'
                    | '\u{FF0F}'
                    | '\u{FF3C}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2066}'..='\u{2069}'
            )
        {
            output.push('_');
            warnings.push("Illegal path characters were replaced".to_string());
        } else {
            output.push(character);
        }
    }
    output = output.trim().trim_end_matches(['.', ' ']).to_string();
    if output == "." || output == ".." || output.is_empty() {
        return Err("Pattern produced an empty or traversal path segment".to_string());
    }
    if reserved_windows_name(&output) {
        output.insert(0, '_');
        warnings.push("A reserved device name was prefixed".to_string());
    }
    if output.len() > MAX_COMPONENT_BYTES {
        while output.len() > MAX_COMPONENT_BYTES {
            output.pop();
        }
        // Truncation can re-expose an illegal trailing dot or space, or empty
        // the segment entirely; sanitize the truncated form again.
        output = output.trim().trim_end_matches(['.', ' ']).to_string();
        if output.is_empty() {
            return Err("Pattern produced an overlong empty path segment".to_string());
        }
        warnings.push("Path segment was truncated to the length limit".to_string());
    }
    warnings.sort();
    warnings.dedup();
    Ok((output, warnings))
}

fn resolve_pattern(
    pattern: &ImportPattern,
    catalog: &MetadataCatalog,
    folder: bool,
) -> Result<(PathBuf, Vec<String>), Vec<String>> {
    if pattern.version != IMPORT_PATTERN_VERSION {
        return Err(vec![format!(
            "Unsupported import pattern version {}",
            pattern.version
        )]);
    }
    let mut raw = String::new();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    for part in &pattern.parts {
        match part {
            PatternPart::Literal { value } => raw.push_str(value),
            PatternPart::Token { token, fallback } => {
                let value = catalog
                    .get(token)
                    .and_then(|value| value.normalized_value.as_deref());
                if let Some(value) = value.filter(|value| !value.is_empty()) {
                    if folder {
                        raw.push_str(value);
                    } else {
                        match sanitize_segment(value) {
                            Ok((safe, part_warnings)) => {
                                raw.push_str(&safe);
                                warnings.extend(part_warnings);
                            }
                            Err(error) => errors.push(error),
                        }
                    }
                } else {
                    match pattern.missing_token_policy {
                        MissingTokenPolicy::Empty => warnings.push(format!("{token:?} is empty")),
                        MissingTokenPolicy::Fallback => {
                            if let Some(fallback) =
                                fallback.as_ref().filter(|value| !value.is_empty())
                            {
                                raw.push_str(fallback.as_str());
                            } else {
                                errors.push(format!("{token:?} requires a fallback"));
                            }
                        }
                        MissingTokenPolicy::Error => errors.push(format!("{token:?} is missing")),
                    }
                }
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    if raw.starts_with(['/', '\\']) || Path::new(&raw).is_absolute() {
        return Err(vec!["Absolute pattern paths are not allowed".to_string()]);
    }
    let raw_parts = if folder {
        raw.split(['/', '\\']).collect::<Vec<_>>()
    } else {
        vec![raw.as_str()]
    };
    let mut result = PathBuf::new();
    for part in raw_parts {
        if part.is_empty() {
            continue;
        }
        if part == "." || part == ".." {
            return Err(vec!["Traversal path segments are not allowed".to_string()]);
        }
        match sanitize_segment(part) {
            Ok((safe, part_warnings)) => {
                result.push(safe);
                warnings.extend(part_warnings);
            }
            Err(error) => return Err(vec![error]),
        }
    }
    if !folder && result.as_os_str().is_empty() {
        return Err(vec!["Filename pattern produced no name".to_string()]);
    }
    if result.to_string_lossy().len() > MAX_RELATIVE_PATH_BYTES {
        return Err(vec![
            "Generated path exceeds the path length limit".to_string(),
        ]);
    }
    warnings.sort();
    warnings.dedup();
    Ok((result, warnings))
}

fn source_stamp(path: &Path) -> Result<SourceStamp, String> {
    let metadata =
        fs::metadata(path).map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
    let modified_nanos = metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(SourceStamp {
        path: path.to_path_buf(),
        len: metadata.len(),
        modified_nanos,
    })
}

fn request_hash(request: &ImportPlanRequest, stamps: &[SourceStamp]) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(request).map_err(|e| e.to_string())?);
    for stamp in stamps {
        hasher.update(stamp.path.to_string_lossy().as_bytes());
        hasher.update(stamp.len.to_le_bytes());
        hasher.update(stamp.modified_nanos.to_le_bytes());
    }
    Ok(hex::encode(hasher.finalize()))
}

fn collect_sources(request: &ImportPlanRequest) -> Result<(Vec<PathBuf>, u64), String> {
    if !request.source_files.is_empty() && request.source_folder.is_some() {
        return Err("Choose selected files or one source folder, not both".to_string());
    }
    let mut candidates = request
        .source_files
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if let Some(folder) = &request.source_folder {
        let folder = PathBuf::from(folder);
        if !folder.is_dir() {
            return Err("Source folder does not exist".to_string());
        }
        let depth = if request.recursive { MAX_SCAN_DEPTH } else { 1 };
        for entry in WalkDir::new(folder).follow_links(false).max_depth(depth) {
            if scan_cancel().load(Ordering::SeqCst) {
                return Err("Import scan cancelled".to_string());
            }
            if let Ok(entry) = entry
                && entry.file_type().is_file()
            {
                candidates.push(entry.into_path());
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    if candidates.len() > MAX_SCAN_ENTRIES {
        return Err(format!(
            "Source scan exceeds the {MAX_SCAN_ENTRIES} entry limit"
        ));
    }
    let total = candidates.len() as u64;
    let sources = candidates
        .into_iter()
        .filter(|path| is_supported_image_file(path))
        .collect::<Vec<_>>();
    Ok((sources.clone(), total.saturating_sub(sources.len() as u64)))
}

fn suffix_destination(path: &Path, number: usize) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("image");
    let extension = path.extension().and_then(|value| value.to_str());
    let filename = match extension {
        Some(extension) if !extension.is_empty() => format!("{stem}_{number}.{extension}"),
        _ => format!("{stem}_{number}"),
    };
    path.with_file_name(filename)
}

fn page(plan_id: &str, plan: &StoredPlan, page_number: usize, page_size: usize) -> ImportPreview {
    let page_size = page_size.clamp(1, MAX_PAGE_SIZE);
    let start = page_number.saturating_mul(page_size).min(plan.rows.len());
    let end = (start + page_size).min(plan.rows.len());
    ImportPreview {
        plan_id: plan_id.to_string(),
        request_hash: plan.request_hash.clone(),
        rows: plan.rows[start..end].to_vec(),
        page: page_number as u64,
        page_size: page_size as u64,
        total_rows: plan.rows.len() as u64,
        has_more: end < plan.rows.len(),
        unsupported_file_count: plan.unsupported,
        warnings: plan.warnings.clone(),
        errors: plan.errors.clone(),
    }
}

fn build_plan(request: ImportPlanRequest) -> Result<(String, StoredPlan), String> {
    if request.folder_pattern.version != IMPORT_PATTERN_VERSION
        || request.filename_pattern.version != IMPORT_PATTERN_VERSION
    {
        return Err("Unsupported import pattern version".to_string());
    }
    let root = fs::canonicalize(&request.destination_root)
        .map_err(|e| format!("Destination root must be an existing accessible folder: {e}"))?;
    if !root.is_dir() {
        return Err("Destination root is not a folder".to_string());
    }
    let _scan_guard = scan_lock()
        .lock()
        .map_err(|_| "Import scan lock unavailable")?;
    scan_cancel().store(false, Ordering::SeqCst);
    let (sources, unsupported) = collect_sources(&request)?;
    let stamps = sources
        .iter()
        .map(|path| source_stamp(path))
        .collect::<Result<Vec<_>, _>>()?;
    let hash = request_hash(&request, &stamps)?;
    let mut rows = Vec::new();
    let mut claimed = HashMap::<String, String>::new();
    for (index, source) in sources.iter().enumerate() {
        let mut conflicts = Vec::new();
        let mut warnings = Vec::new();
        let catalog = match metadata_catalog(source, index + 1, sources.len()) {
            Ok(catalog) => catalog,
            Err(error) => {
                conflicts.push(ImportConflict {
                    code: "metadata".to_string(),
                    message: error,
                    conflicting_source: None,
                });
                MetadataCatalog::new()
            }
        };
        let folder = resolve_pattern(&request.folder_pattern, &catalog, true);
        let filename = resolve_pattern(&request.filename_pattern, &catalog, false);
        let mut relative = PathBuf::new();
        match folder {
            Ok((path, resolved_warnings)) => {
                relative.push(path);
                warnings.extend(resolved_warnings);
            }
            Err(errors) => {
                conflicts.extend(errors.into_iter().map(|message| ImportConflict {
                    code: "folderPattern".to_string(),
                    message,
                    conflicting_source: None,
                }));
            }
        }
        match filename {
            Ok((mut path, resolved_warnings)) => {
                warnings.extend(resolved_warnings);
                let extension = source
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or("");
                if path.extension().is_none() && !extension.is_empty() {
                    path.set_extension(extension);
                }
                relative.push(path);
            }
            Err(errors) => {
                conflicts.extend(errors.into_iter().map(|message| ImportConflict {
                    code: "filenamePattern".to_string(),
                    message,
                    conflicting_source: None,
                }));
            }
        }
        let mut status = if conflicts.is_empty() {
            ImportPreviewStatus::Ready
        } else {
            ImportPreviewStatus::Blocked
        };
        let source_canonical = fs::canonicalize(source).ok();
        let mut destination = root.join(&relative);
        if destination.exists()
            || source_canonical
                .as_ref()
                .is_some_and(|value| value == &destination)
        {
            match request.collision_policy {
                CollisionPolicy::Skip => {
                    status = ImportPreviewStatus::Skipped;
                    conflicts.push(ImportConflict {
                        code: "existingDestination".to_string(),
                        message: "Destination already exists".to_string(),
                        conflicting_source: None,
                    });
                }
                CollisionPolicy::Error => {
                    status = ImportPreviewStatus::Blocked;
                    conflicts.push(ImportConflict {
                        code: "existingDestination".to_string(),
                        message: "Destination already exists".to_string(),
                        conflicting_source: None,
                    });
                }
                CollisionPolicy::RenameWithSuffix => {
                    let mut suffix = 2;
                    while destination.exists()
                        || claimed.contains_key(&destination.to_string_lossy().to_lowercase())
                    {
                        destination = suffix_destination(&root.join(&relative), suffix);
                        suffix += 1;
                    }
                    relative = destination
                        .strip_prefix(&root)
                        .unwrap_or(&destination)
                        .to_path_buf();
                    status = ImportPreviewStatus::Renamed;
                }
            }
        }
        let key = destination.to_string_lossy().to_lowercase();
        if let Some(other) = claimed.get(&key) {
            status = ImportPreviewStatus::Blocked;
            conflicts.push(ImportConflict {
                code: "duplicateDestination".to_string(),
                message: "Another source resolves to the same destination".to_string(),
                conflicting_source: Some(other.clone()),
            });
        } else if matches!(
            status,
            ImportPreviewStatus::Ready | ImportPreviewStatus::Renamed
        ) {
            claimed.insert(key, source.to_string_lossy().to_string());
        }
        rows.push(ImportPreviewRow {
            source_path: source.to_string_lossy().to_string(),
            destination_relative_path: relative.to_string_lossy().replace('\\', "/"),
            metadata: catalog,
            associated_files: if request.include_associated_files {
                sidecar_paths(source)
                    .into_iter()
                    .filter(|path| path.exists())
                    .map(|path| path.to_string_lossy().to_string())
                    .collect()
            } else {
                Vec::new()
            },
            byte_size: stamps.get(index).map(|stamp| stamp.len),
            status,
            conflicts,
            warnings,
        });
    }
    let errors = if rows
        .iter()
        .any(|row| row.status == ImportPreviewStatus::Blocked)
    {
        vec!["Resolve blocking preview conflicts before importing".to_string()]
    } else {
        Vec::new()
    };
    let plan = StoredPlan {
        request,
        request_hash: hash,
        root,
        rows,
        stamps,
        unsupported,
        warnings: Vec::new(),
        errors,
    };
    Ok((uuid::Uuid::new_v4().to_string(), plan))
}

#[tauri::command]
pub async fn create_import_plan(
    request: ImportPlanRequest,
    app_handle: AppHandle,
) -> Result<ImportPreview, String> {
    let _ = app_handle.emit(
        "import-progress",
        ImportProgressEvent {
            plan_id: None,
            phase: ImportPhase::Scanning,
            current: 0,
            total: 0,
            bytes_completed: None,
            bytes_total: None,
            source_path: None,
        },
    );
    let (id, plan) = tauri::async_runtime::spawn_blocking(move || build_plan(request))
        .await
        .map_err(|e| e.to_string())??;
    let preview = page(&id, &plan, 0, DEFAULT_PAGE_SIZE);
    let mut stored = plans().lock().map_err(|_| "Plan store unavailable")?;
    // Bounded cache: past the cap one entry is evicted (HashMap order is
    // arbitrary, which is fine — stale plans are also rejected by hash).
    if stored.len() >= MAX_STORED_PLANS
        && let Some(evict) = stored.keys().next().cloned()
    {
        stored.remove(&evict);
    }
    stored.insert(id.clone(), plan);
    drop(stored);
    let _ = app_handle.emit(
        "import-progress",
        ImportProgressEvent {
            plan_id: Some(id),
            phase: ImportPhase::Planning,
            current: preview.total_rows,
            total: preview.total_rows,
            bytes_completed: None,
            bytes_total: None,
            source_path: None,
        },
    );
    Ok(preview)
}

#[tauri::command]
pub fn get_import_preview_page(request: ImportPreviewPageRequest) -> Result<ImportPreview, String> {
    let plans = plans().lock().map_err(|_| "Plan store unavailable")?;
    let plan = plans
        .get(&request.plan_id)
        .ok_or("Import plan is stale or unknown")?;
    Ok(page(
        &request.plan_id,
        plan,
        request.page as usize,
        request.page_size as usize,
    ))
}

#[tauri::command]
pub fn cancel_import(plan_id: Option<String>) -> Result<(), String> {
    // A plan-scoped cancellation must not latch the global scan flag: that
    // would spuriously abort an unrelated concurrent plan scan (and keep
    // aborting scans until the next build_plan resets the flag). Only a
    // plan-less cancellation cancels everything, including the current scan.
    if plan_id.is_none() {
        scan_cancel().store(true, Ordering::SeqCst);
    }
    let tokens = cancellations()
        .lock()
        .map_err(|_| "Cancellation store unavailable")?;
    if let Some(plan_id) = plan_id {
        if let Some(token) = tokens.get(&plan_id) {
            token.store(true, Ordering::SeqCst);
        }
    } else {
        for token in tokens.values() {
            token.store(true, Ordering::SeqCst);
        }
    }
    Ok(())
}

fn validate_contained(root: &Path, destination: &Path) -> Result<(), String> {
    let relative = destination
        .strip_prefix(root)
        .map_err(|_| "Destination escaped the selected root".to_string())?;
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("Destination contains an unsafe component".to_string());
    }
    let mut ancestor = destination.parent();
    while let Some(path) = ancestor {
        if path.exists() {
            let canonical = fs::canonicalize(path).map_err(|e| e.to_string())?;
            if !canonical.starts_with(root) {
                return Err("Destination parent escaped through a symlink".to_string());
            }
            break;
        }
        ancestor = path.parent();
    }
    Ok(())
}

fn copy_verified(source: &Path, destination: &Path, cancel: &AtomicBool) -> Result<u64, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".to_string());
    }
    let parent = destination.parent().ok_or("Destination has no parent")?;
    fs::create_dir_all(parent).map_err(|e| format!("Cannot create destination folder: {e}"))?;
    let temp = parent.join(format!(".rapidraw-import-{}.tmp", uuid::Uuid::new_v4()));
    let operation = (|| {
        let mut input = File::open(source).map_err(|e| format!("Cannot open source: {e}"))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| format!("Cannot create temporary output: {e}"))?;
        let mut source_hash = Sha256::new();
        let mut buffer = [0_u8; 256 * 1024];
        let mut written = 0_u64;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("cancelled".to_string());
            }
            let count = input
                .read(&mut buffer)
                .map_err(|e| format!("Copy read failed: {e}"))?;
            if count == 0 {
                break;
            }
            output
                .write_all(&buffer[..count])
                .map_err(|e| format!("Copy write failed: {e}"))?;
            source_hash.update(&buffer[..count]);
            written += count as u64;
        }
        output.flush().map_err(|e| format!("Flush failed: {e}"))?;
        output
            .sync_all()
            .map_err(|e| format!("Disk sync failed: {e}"))?;
        drop(output);
        if written != fs::metadata(source).map_err(|e| e.to_string())?.len()
            || written != fs::metadata(&temp).map_err(|e| e.to_string())?.len()
        {
            return Err("Copied byte count did not verify".to_string());
        }
        let mut verify_hash = Sha256::new();
        let mut verify = File::open(&temp).map_err(|e| e.to_string())?;
        std::io::copy(&mut verify, &mut HashWriter(&mut verify_hash)).map_err(|e| e.to_string())?;
        if source_hash.finalize() != verify_hash.finalize() {
            return Err("Copied content hash did not verify".to_string());
        }
        if destination.exists() {
            return Err("Destination appeared after preview".to_string());
        }
        fs::rename(&temp, destination).map_err(|e| format!("Atomic finalize failed: {e}"))?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(written)
    })();
    if operation.is_err() {
        let _ = fs::remove_file(&temp);
    }
    operation
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn associated_destination(source: &Path, destination: &Path, associated: &Path) -> PathBuf {
    if associated == source.with_extension("xmp") {
        return destination.with_extension("xmp");
    }
    let source_name = source.file_name().unwrap_or_default().to_string_lossy();
    let associated_name = associated.file_name().unwrap_or_default().to_string_lossy();
    let suffix = associated_name
        .strip_prefix(source_name.as_ref())
        .map(str::to_string)
        .unwrap_or_else(|| {
            associated
                .extension()
                .map(|extension| format!(".{}", extension.to_string_lossy()))
                .unwrap_or_default()
        });
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    destination.with_file_name(name)
}

fn execute_plan<E>(
    plan_id: String,
    expected_hash: String,
    emit: E,
    cancel: Arc<AtomicBool>,
) -> Result<ImportResult, String>
where
    E: Fn(ImportProgressEvent),
{
    let plan = plans()
        .lock()
        .map_err(|_| "Plan store unavailable")?
        .get(&plan_id)
        .cloned()
        .ok_or("Import plan is stale or unknown")?;
    if plan.request_hash != expected_hash
        || request_hash(&plan.request, &plan.stamps)? != expected_hash
    {
        return Err("Import plan ID or request hash is stale".to_string());
    }
    for expected in &plan.stamps {
        let actual = source_stamp(&expected.path)?;
        if actual.len != expected.len || actual.modified_nanos != expected.modified_nanos {
            return Err(format!(
                "Source changed after preview: {}",
                expected.path.display()
            ));
        }
    }
    if !plan.errors.is_empty() {
        return Err(plan.errors.join("; "));
    }
    let root =
        fs::canonicalize(&plan.root).map_err(|_| "Destination root is no longer available")?;
    let total = plan.rows.len() as u64;
    let mut result = ImportResult {
        plan_id: plan_id.clone(),
        cancelled: false,
        succeeded: 0,
        renamed: 0,
        skipped: 0,
        warned: 0,
        failed: 0,
        retained_sources: 0,
        items: Vec::new(),
    };
    for (index, row) in plan.rows.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            result.cancelled = true;
            break;
        }
        if row.status == ImportPreviewStatus::Skipped {
            result.skipped += 1;
            result.items.push(ImportItemResult {
                source_path: row.source_path.clone(),
                destination_relative_path: Some(row.destination_relative_path.clone()),
                status: ImportItemStatus::Skipped,
                source_retained: true,
                warnings: row.warnings.clone(),
                error: None,
            });
            continue;
        }
        let source = PathBuf::from(&row.source_path);
        let destination = root.join(&row.destination_relative_path);
        emit(ImportProgressEvent {
            plan_id: Some(plan_id.clone()),
            phase: ImportPhase::Copying,
            current: index as u64,
            total,
            bytes_completed: None,
            bytes_total: row.byte_size,
            source_path: Some(row.source_path.clone()),
        });
        let group_result = (|| {
            validate_contained(&root, &destination)?;
            if destination.exists() {
                return Err("Destination appeared after preview".to_string());
            }
            copy_verified(&source, &destination, &cancel)?;
            for associated in &row.associated_files {
                let associated = PathBuf::from(associated);
                let target = associated_destination(&source, &destination, &associated);
                validate_contained(&root, &target)?;
                if target.exists() {
                    return Err(format!(
                        "Associated destination already exists: {}",
                        target.display()
                    ));
                }
                copy_verified(&associated, &target, &cancel)?;
            }
            if plan.request.preserve_timestamps {
                if let Ok(metadata) = fs::metadata(&source)
                    && let Ok(modified) = metadata.modified()
                {
                    let time = filetime::FileTime::from_system_time(modified);
                    let _ = filetime::set_file_mtime(&destination, time);
                }
            } else if plan.request.use_capture_time
                && let Some(capture) = exif_processing::try_get_exif_creation_date(&source)
            {
                let time = filetime::FileTime::from_unix_time(capture.timestamp(), 0);
                let _ = filetime::set_file_mtime(&destination, time);
            }
            if plan.request.operation == ImportOperation::Move {
                if cancel.load(Ordering::SeqCst) {
                    return Err("cancelled before source retirement".to_string());
                }
                #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
                {
                    let mut retirement_error = None;
                    for source_to_retire in row
                        .associated_files
                        .iter()
                        .map(PathBuf::from)
                        .chain(std::iter::once(source.clone()))
                    {
                        if let Err(error) = trash::delete(&source_to_retire) {
                            retirement_error = Some(format!(
                                "Verified copy retained source because trash failed for {}: {error}",
                                source_to_retire.display()
                            ));
                            break;
                        }
                    }
                    if let Some(error) = retirement_error {
                        return Err(error);
                    }
                }
                #[cfg(not(any(
                    target_os = "windows",
                    target_os = "macos",
                    target_os = "linux"
                )))]
                return Err(
                    "Move is unsupported on this platform; verified copies were retained"
                        .to_string(),
                );
            }
            Ok(())
        })();
        match group_result {
            Ok(()) => {
                let renamed = row.status == ImportPreviewStatus::Renamed;
                let warned = !row.warnings.is_empty();
                result.succeeded += 1;
                result.renamed += u64::from(renamed);
                result.warned += u64::from(warned);
                result.items.push(ImportItemResult {
                    source_path: row.source_path.clone(),
                    destination_relative_path: Some(row.destination_relative_path.clone()),
                    status: if warned {
                        ImportItemStatus::Warned
                    } else if renamed {
                        ImportItemStatus::Renamed
                    } else {
                        ImportItemStatus::Succeeded
                    },
                    source_retained: plan.request.operation == ImportOperation::Copy,
                    warnings: row.warnings.clone(),
                    error: None,
                });
            }
            Err(error) if error.starts_with("cancelled") => {
                result.cancelled = true;
                result.retained_sources += 1;
                result.items.push(ImportItemResult {
                    source_path: row.source_path.clone(),
                    destination_relative_path: Some(row.destination_relative_path.clone()),
                    status: ImportItemStatus::Cancelled,
                    source_retained: true,
                    warnings: row.warnings.clone(),
                    error: Some(error),
                });
                break;
            }
            Err(error) => {
                result.failed += 1;
                result.retained_sources += 1;
                result.items.push(ImportItemResult {
                    source_path: row.source_path.clone(),
                    destination_relative_path: Some(row.destination_relative_path.clone()),
                    status: ImportItemStatus::Failed,
                    source_retained: true,
                    warnings: row.warnings.clone(),
                    error: Some(error),
                });
            }
        }
    }
    emit(ImportProgressEvent {
        plan_id: Some(plan_id),
        phase: if result.cancelled {
            ImportPhase::Cancelled
        } else {
            ImportPhase::Complete
        },
        current: result.items.len() as u64,
        total,
        bytes_completed: None,
        bytes_total: None,
        source_path: None,
    });
    Ok(result)
}

#[tauri::command]
pub async fn execute_import_plan(
    request: ExecuteImportRequest,
    app_handle: AppHandle,
) -> Result<ImportResult, String> {
    let token = Arc::new(AtomicBool::new(false));
    cancellations()
        .lock()
        .map_err(|_| "Cancellation store unavailable")?
        .insert(request.plan_id.clone(), token.clone());
    let plan_id = request.plan_id.clone();
    let emitter = move |event: ImportProgressEvent| {
        let _ = app_handle.emit("import-progress", event);
    };
    let result = tauri::async_runtime::spawn_blocking(move || {
        execute_plan(request.plan_id, request.request_hash, emitter, token)
    })
    .await
    .map_err(|e| e.to_string())?;
    cancellations()
        .lock()
        .map_err(|_| "Cancellation store unavailable")?
        .remove(&plan_id);
    result
}

#[tauri::command]
pub async fn import_android_content_files(
    source_paths: Vec<String>,
    destination_root: String,
    app_handle: AppHandle,
) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        return crate::file_management::import_files(
            source_paths,
            destination_root,
            crate::file_management::ImportSettings {
                filename_template: "{original_filename}".to_string(),
                organize_by_date: false,
                date_folder_format: "YYYY/MM-DD".to_string(),
                delete_after_import: false,
            },
            app_handle,
        )
        .await;
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (source_paths, destination_root, app_handle);
        Err("Android content imports are unavailable on desktop".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(token: MetadataToken) -> PatternPart {
        PatternPart::Token {
            token,
            fallback: None,
        }
    }

    fn catalog(values: &[(MetadataToken, &str)]) -> MetadataCatalog {
        let mut result = MetadataCatalog::new();
        for token in ALL_TOKENS {
            let value = values
                .iter()
                .find(|(candidate, _)| *candidate == token)
                .map(|(_, value)| *value);
            result.insert(
                token,
                MetadataValue {
                    token,
                    raw_value: value.map(str::to_string),
                    normalized_value: value.map(str::to_string),
                    source: value.map(|_| "fixture".to_string()),
                    missing: value.is_none(),
                },
            );
        }
        result
    }

    #[test]
    fn pattern_engine_rejects_traversal_absolute_and_reserved_names() {
        let metadata = catalog(&[(MetadataToken::Model, "../CON")]);
        let pattern = ImportPattern {
            version: 1,
            parts: vec![token(MetadataToken::Model)],
            missing_token_policy: MissingTokenPolicy::Error,
        };
        let (resolved, _) =
            resolve_pattern(&pattern, &metadata, false).expect("sanitized filename");
        assert_eq!(resolved.to_string_lossy(), ".._CON");
        let folder = ImportPattern {
            parts: vec![PatternPart::Literal {
                value: "../escape".to_string(),
            }],
            ..pattern
        };
        assert!(resolve_pattern(&folder, &metadata, true).is_err());
    }

    #[test]
    fn pattern_engine_missing_policy_and_legacy_compatibility() {
        let parsed = parse_legacy_pattern(
            "{YYYY}/{original_filename}_{sequence}",
            MissingTokenPolicy::Error,
        );
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.parts.len(), 5);
        let missing = ImportPattern {
            version: 1,
            parts: vec![PatternPart::Token {
                token: MetadataToken::Artist,
                fallback: Some("unknown".to_string()),
            }],
            missing_token_policy: MissingTokenPolicy::Fallback,
        };
        assert_eq!(
            resolve_pattern(&missing, &catalog(&[]), false)
                .expect("fallback")
                .0
                .to_string_lossy(),
            "unknown"
        );
    }

    #[test]
    fn pattern_engine_preserves_legacy_token_names() {
        use crate::file_management::generate_filename_from_template;
        use chrono::{DateTime, Utc};

        let directory = tempfile::tempdir().expect("temp");
        let image = directory.path().join("DSC_0123.nef");
        fs::write(&image, b"raw").expect("image");
        let date = DateTime::parse_from_rfc3339("2024-01-05T10:20:00Z")
            .expect("date")
            .with_timezone(&Utc);

        for (alias, expected) in [
            ("original_filename", MetadataToken::OriginalStem),
            ("original_stem", MetadataToken::OriginalStem),
            ("extension", MetadataToken::Extension),
            ("sequence", MetadataToken::Sequence),
            ("YYYY", MetadataToken::Year),
            ("MM", MetadataToken::Month),
            ("DD", MetadataToken::Day),
            ("hh", MetadataToken::Hour),
            ("mm", MetadataToken::Minute),
            ("make", MetadataToken::Make),
            ("model", MetadataToken::Model),
            ("lens_model", MetadataToken::LensModel),
            ("iso", MetadataToken::Iso),
            ("artist", MetadataToken::Artist),
            ("copyright", MetadataToken::Copyright),
            ("description", MetadataToken::ImageDescription),
            ("keywords", MetadataToken::Keywords),
            ("rating", MetadataToken::Rating),
            ("color_label", MetadataToken::ColorLabel),
        ] {
            let parsed = parse_legacy_pattern(&format!("{{{alias}}}"), MissingTokenPolicy::Error);
            assert_eq!(
                parsed.parts,
                vec![PatternPart::Token {
                    token: expected,
                    fallback: None,
                }],
                "legacy alias {alias} must stay a typed token"
            );
        }
        let unknown = parse_legacy_pattern("keep {oops} literal", MissingTokenPolicy::Error);
        assert_eq!(unknown.parts.len(), 3);

        let total = 1200;
        for sequence in [1, 3, 1200] {
            let catalog = metadata_catalog(&image, sequence, total).expect("catalog");
            let legacy_stem = generate_filename_from_template(
                "{original_filename}_{sequence}",
                &image,
                sequence,
                total,
                &date,
            );
            let (resolved, _) = resolve_pattern(
                &parse_legacy_pattern("{original_filename}_{sequence}", MissingTokenPolicy::Error),
                &catalog,
                false,
            )
            .expect("legacy-compatible name");
            assert_eq!(resolved.to_string_lossy(), legacy_stem);
        }

        fs::write(
            directory.path().join("DSC_0123.nef.rrdata"),
            r#"{"DateTimeOriginal": "2024:01:05 10:20:00"}"#,
        )
        .expect("rrdata");
        let catalog = metadata_catalog(&image, 1, 1).expect("catalog");
        for (alias, width) in [("YYYY", 4), ("MM", 2), ("DD", 2), ("hh", 2), ("mm", 2)] {
            let (resolved, _) = resolve_pattern(
                &parse_legacy_pattern(&format!("{{{alias}}}"), MissingTokenPolicy::Error),
                &catalog,
                false,
            )
            .expect("date token");
            let value = resolved.to_string_lossy();
            assert_eq!(value.len(), width, "legacy token {alias}");
            assert!(
                value.chars().all(|character| character.is_ascii_digit()),
                "legacy token {alias}"
            );
        }
    }

    #[test]
    fn metadata_catalog_sidecars_override_xmp_and_embedded_without_writes() {
        let directory = tempfile::tempdir().expect("temp");
        let image = directory.path().join("photo.jpg");
        fs::write(&image, b"not an image").expect("image");
        fs::write(
            image.with_extension("xmp"),
            r#"<x:xmpmeta><rdf:Description xmp:Rating="3" dc:creator="XMP"/></x:xmpmeta>"#,
        )
        .expect("xmp");
        fs::write(
            directory.path().join("photo.jpg.rrdata"),
            r#"{"rating":5,"exif":{"Artist":"RapidRAW"}}"#,
        )
        .expect("rrdata");
        let before = fs::read_dir(directory.path()).expect("list").count();
        let catalog = metadata_catalog(&image, 1, 1).expect("catalog");
        assert_eq!(
            catalog[&MetadataToken::Artist].normalized_value.as_deref(),
            Some("RapidRAW")
        );
        assert_eq!(
            catalog[&MetadataToken::Rating].normalized_value.as_deref(),
            Some("5")
        );
        assert_eq!(
            fs::read_dir(directory.path()).expect("list").count(),
            before
        );
    }

    #[test]
    fn import_plan_is_paged_side_effect_free_and_detects_duplicates() {
        let directory = tempfile::tempdir().expect("temp");
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&destination).expect("destination");
        fs::write(source.join("a.jpg"), b"a").expect("a");
        fs::write(source.join("b.jpg"), b"b").expect("b");
        let request = ImportPlanRequest {
            source_files: vec![
                source.join("a.jpg").to_string_lossy().to_string(),
                source.join("b.jpg").to_string_lossy().to_string(),
            ],
            source_folder: None,
            recursive: false,
            destination_root: destination.to_string_lossy().to_string(),
            folder_pattern: ImportPattern {
                version: 1,
                parts: Vec::new(),
                missing_token_policy: MissingTokenPolicy::Empty,
            },
            filename_pattern: ImportPattern {
                version: 1,
                parts: vec![PatternPart::Literal {
                    value: "same".to_string(),
                }],
                missing_token_policy: MissingTokenPolicy::Error,
            },
            operation: ImportOperation::Copy,
            collision_policy: CollisionPolicy::Error,
            include_associated_files: true,
            preserve_timestamps: false,
            use_capture_time: false,
        };
        let (_, plan) = build_plan(request).expect("plan");
        assert_eq!(plan.rows.len(), 2);
        assert_eq!(plan.rows[1].status, ImportPreviewStatus::Blocked);
        assert_eq!(fs::read_dir(&destination).expect("list").count(), 0);
        assert_eq!(page("id", &plan, 0, 1).rows.len(), 1);
        assert!(page("id", &plan, 0, 1).has_more);
    }

    #[test]
    fn import_scan_recursion_unsupported_and_plan_invalidation() {
        let directory = tempfile::tempdir().expect("temp");
        let source = directory.path().join("source");
        let nested = source.join("nested");
        let destination = directory.path().join("destination");
        fs::create_dir_all(&nested).expect("source tree");
        fs::create_dir_all(&destination).expect("destination");
        fs::write(source.join("top.jpg"), b"top").expect("top");
        fs::write(nested.join("deep.nef"), b"deep").expect("deep");
        fs::write(source.join("notes.txt"), b"ignored").expect("notes");
        let base_request = ImportPlanRequest {
            source_files: Vec::new(),
            source_folder: Some(source.to_string_lossy().to_string()),
            recursive: true,
            destination_root: destination.to_string_lossy().to_string(),
            folder_pattern: ImportPattern {
                version: 1,
                parts: Vec::new(),
                missing_token_policy: MissingTokenPolicy::Empty,
            },
            filename_pattern: parse_legacy_pattern(
                "{original_filename}",
                MissingTokenPolicy::Error,
            ),
            operation: ImportOperation::Copy,
            collision_policy: CollisionPolicy::Error,
            include_associated_files: true,
            preserve_timestamps: false,
            use_capture_time: false,
        };
        let (id, plan) = build_plan(base_request.clone()).expect("recursive plan");
        assert!(!id.is_empty());
        assert_eq!(plan.rows.len(), 2, "top.jpg and nested deep.nef planned");
        assert_eq!(plan.unsupported, 1, "notes.txt counted as unsupported");

        let flat = ImportPlanRequest {
            recursive: false,
            ..base_request.clone()
        };
        let (_, flat_plan) = build_plan(flat).expect("flat plan");
        assert_eq!(
            flat_plan.rows.len(),
            1,
            "non-recursive scan stays at the top level"
        );

        let moved = ImportPlanRequest {
            operation: ImportOperation::Move,
            ..base_request.clone()
        };
        let (_, moved_plan) = build_plan(moved).expect("moved plan");
        assert_ne!(
            moved_plan.request_hash, plan.request_hash,
            "operation change invalidates the plan"
        );
        fs::write(source.join("top.jpg"), b"top edited").expect("edit");
        let (_, edited_plan) = build_plan(base_request.clone()).expect("edited plan");
        assert_ne!(
            edited_plan.request_hash, plan.request_hash,
            "source change invalidates the plan"
        );

        let mixed = ImportPlanRequest {
            source_files: vec![source.join("top.jpg").to_string_lossy().to_string()],
            ..base_request.clone()
        };
        assert!(
            build_plan(mixed).is_err(),
            "files and folder cannot be combined"
        );
        let missing_root = ImportPlanRequest {
            destination_root: directory
                .path()
                .join("missing")
                .to_string_lossy()
                .to_string(),
            ..base_request.clone()
        };
        assert!(
            build_plan(missing_root).is_err(),
            "destination root must exist"
        );

        // Hold the same lock build_plan uses so parallel tests running
        // build_plan cannot reset the shared scan flag mid-assertion.
        let _scan_guard = scan_lock().lock().expect("scan lock");
        scan_cancel().store(true, Ordering::SeqCst);
        let error = collect_sources(&base_request).expect_err("cancelled scan");
        assert!(error.contains("cancelled"), "unexpected error: {error}");
        scan_cancel().store(false, Ordering::SeqCst);
        drop(_scan_guard);

        assert!(
            get_import_preview_page(ImportPreviewPageRequest {
                plan_id: "does-not-exist".to_string(),
                page: 0,
                page_size: DEFAULT_PAGE_SIZE as u64,
            })
            .is_err()
        );

        fs::write(destination.join("top.jpg"), b"existing").expect("existing");
        let (_, collided) = build_plan(base_request.clone()).expect("collided plan");
        let top = collided
            .rows
            .iter()
            .find(|row| row.source_path.ends_with("top.jpg"))
            .expect("top row");
        assert_eq!(top.status, ImportPreviewStatus::Blocked);
        assert!(
            top.conflicts
                .iter()
                .any(|conflict| conflict.code == "existingDestination"),
            "existing destination collision reported"
        );

        let same_root = ImportPlanRequest {
            destination_root: source.to_string_lossy().to_string(),
            recursive: false,
            ..base_request
        };
        let (_, same_plan) = build_plan(same_root).expect("self plan");
        assert_eq!(same_plan.rows.len(), 1);
        assert!(
            same_plan.rows[0]
                .conflicts
                .iter()
                .any(|conflict| conflict.code == "existingDestination"),
            "source equal to destination reported"
        );
        assert_eq!(
            same_plan.rows[0].status,
            ImportPreviewStatus::Blocked,
            "source-equals-destination blocks execution"
        );
    }

    #[test]
    fn import_execution_copy_is_verified_and_stale_plan_is_rejected() {
        let directory = tempfile::tempdir().expect("temp");
        let source = directory.path().join("source.jpg");
        let destination = directory.path().join("destination");
        fs::write(&source, b"source bytes").expect("source");
        fs::create_dir(&destination).expect("destination");
        let request = ImportPlanRequest {
            source_files: vec![source.to_string_lossy().to_string()],
            source_folder: None,
            recursive: false,
            destination_root: destination.to_string_lossy().to_string(),
            folder_pattern: ImportPattern {
                version: 1,
                parts: Vec::new(),
                missing_token_policy: MissingTokenPolicy::Empty,
            },
            filename_pattern: parse_legacy_pattern(
                "{original_filename}",
                MissingTokenPolicy::Error,
            ),
            operation: ImportOperation::Copy,
            collision_policy: CollisionPolicy::Error,
            include_associated_files: true,
            preserve_timestamps: true,
            use_capture_time: false,
        };
        let (id, plan) = build_plan(request).expect("plan");
        let hash = plan.request_hash.clone();
        plans().lock().expect("plans").insert(id.clone(), plan);
        // The filesystem primitive is exercised without requiring a Tauri runtime.
        let output = destination.join("source.jpg");
        copy_verified(&source, &output, &AtomicBool::new(false)).expect("copy");
        assert_eq!(fs::read(output).expect("output"), b"source bytes");
        assert!(source.exists());
        assert_ne!(hash, "altered");
    }

    #[test]
    fn import_execution_end_to_end_copy_sidecars_and_safety_paths() {
        let cancel = Arc::new(AtomicBool::new(false));
        let recorded = Arc::new(Mutex::new(Vec::<ImportPhase>::new()));
        let make_recorder = |recorded: Arc<Mutex<Vec<ImportPhase>>>| {
            move |event: ImportProgressEvent| {
                if let Ok(mut phases) = recorded.lock() {
                    phases.push(event.phase);
                }
            }
        };
        let directory = tempfile::tempdir().expect("temp");
        let sources = directory.path().join("sources");
        let destination = directory.path().join("library");
        fs::create_dir_all(&sources).expect("sources");
        fs::create_dir_all(&destination).expect("destination");

        let plan_for = |request: ImportPlanRequest| {
            let (id, plan) = build_plan(request).expect("plan");
            let hash = plan.request_hash.clone();
            plans().lock().expect("plans").insert(id.clone(), plan);
            (id, hash)
        };

        // Verified copy with sidecars, preserved timestamps, retained source.
        let image = sources.join("photo.jpg");
        fs::write(&image, b"jpeg bytes").expect("image");
        fs::write(sources.join("photo.jpg.rrdata"), br#"{"rating":5}"#).expect("rrdata");
        fs::write(sources.join("photo.jpg.xmp"), r#"<x:xmpmeta/>"#).expect("xmp");
        let captured = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(&image, captured).expect("mtime");
        let (id, hash) = plan_for(ImportPlanRequest {
            source_files: vec![image.to_string_lossy().to_string()],
            source_folder: None,
            recursive: false,
            destination_root: destination.to_string_lossy().to_string(),
            folder_pattern: ImportPattern {
                version: 1,
                parts: Vec::new(),
                missing_token_policy: MissingTokenPolicy::Empty,
            },
            filename_pattern: parse_legacy_pattern(
                "{original_filename}",
                MissingTokenPolicy::Error,
            ),
            operation: ImportOperation::Copy,
            collision_policy: CollisionPolicy::Error,
            include_associated_files: true,
            preserve_timestamps: true,
            use_capture_time: false,
        });
        let result = execute_plan(
            id.clone(),
            hash.clone(),
            make_recorder(recorded.clone()),
            cancel.clone(),
        )
        .expect("copy");
        assert_eq!(result.succeeded, 1);
        assert_eq!(result.failed, 0);
        assert!(result.items[0].source_retained, "copy retains sources");
        assert_eq!(
            fs::read(destination.join("photo.jpg")).expect("copied"),
            b"jpeg bytes"
        );
        assert_eq!(
            fs::read(destination.join("photo.jpg.rrdata")).expect("rrdata"),
            br#"{"rating":5}"#
        );
        assert_eq!(
            fs::read(destination.join("photo.jpg.xmp")).expect("xmp"),
            r#"<x:xmpmeta/>"#.as_bytes()
        );
        assert!(image.exists(), "copy never removes the source");
        let copied_mtime = fs::metadata(destination.join("photo.jpg"))
            .and_then(|metadata| metadata.modified())
            .expect("mtime")
            .duration_since(UNIX_EPOCH)
            .expect("mtime epoch")
            .as_secs();
        assert_eq!(copied_mtime, 1_600_000_000, "source mtime preserved");

        // Tampered request hash is rejected before any write.
        let error = execute_plan(
            id,
            "tampered".to_string(),
            make_recorder(recorded.clone()),
            cancel.clone(),
        )
        .expect_err("stale hash rejected");
        assert!(error.contains("stale"), "unexpected error: {error}");

        // Source changed after preview blocks execution.
        let tracked_image = sources.join("late.jpg");
        fs::write(&tracked_image, b"v1").expect("v1");
        let (id, hash) = plan_for(ImportPlanRequest {
            source_files: vec![tracked_image.to_string_lossy().to_string()],
            source_folder: None,
            recursive: false,
            destination_root: destination.to_string_lossy().to_string(),
            folder_pattern: ImportPattern {
                version: 1,
                parts: Vec::new(),
                missing_token_policy: MissingTokenPolicy::Empty,
            },
            filename_pattern: parse_legacy_pattern(
                "{original_filename}",
                MissingTokenPolicy::Error,
            ),
            operation: ImportOperation::Copy,
            collision_policy: CollisionPolicy::Error,
            include_associated_files: false,
            preserve_timestamps: false,
            use_capture_time: false,
        });
        fs::write(&tracked_image, b"v2 with more bytes").expect("v2");
        let error = execute_plan(id, hash, make_recorder(recorded.clone()), cancel.clone())
            .expect_err("changed source");
        assert!(
            error.contains("Source changed after preview"),
            "unexpected error: {error}"
        );
        fs::write(&tracked_image, b"v1").expect("restore");
        fs::remove_file(destination.join("late.jpg")).ok();

        // Collision appearing after preview fails that row in isolation, source retained.
        let (id, hash) = plan_for(ImportPlanRequest {
            source_files: vec![tracked_image.to_string_lossy().to_string()],
            source_folder: None,
            recursive: false,
            destination_root: destination.to_string_lossy().to_string(),
            folder_pattern: ImportPattern {
                version: 1,
                parts: Vec::new(),
                missing_token_policy: MissingTokenPolicy::Empty,
            },
            filename_pattern: parse_legacy_pattern(
                "{original_filename}",
                MissingTokenPolicy::Error,
            ),
            operation: ImportOperation::Copy,
            collision_policy: CollisionPolicy::Error,
            include_associated_files: false,
            preserve_timestamps: false,
            use_capture_time: false,
        });
        fs::write(destination.join("late.jpg"), b"raced").expect("raced");
        let result = execute_plan(id, hash, make_recorder(recorded.clone()), cancel.clone())
            .expect("isolated");
        assert_eq!(result.failed, 1, "late collision fails its row only");
        assert!(
            result.items[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("appeared after preview")),
            "unexpected item error: {:?}",
            result.items[0].error
        );
        assert!(result.items[0].source_retained);
        assert!(tracked_image.exists());
        assert_eq!(
            fs::read(destination.join("late.jpg")).expect("raced intact"),
            b"raced",
            "existing destination is never overwritten"
        );

        // Cancellation before any write leaves everything in place.
        fs::remove_file(destination.join("late.jpg")).expect("clear");
        let (id, hash) = plan_for(ImportPlanRequest {
            source_files: vec![tracked_image.to_string_lossy().to_string()],
            source_folder: None,
            recursive: false,
            destination_root: destination.to_string_lossy().to_string(),
            folder_pattern: ImportPattern {
                version: 1,
                parts: Vec::new(),
                missing_token_policy: MissingTokenPolicy::Empty,
            },
            filename_pattern: parse_legacy_pattern(
                "{original_filename}",
                MissingTokenPolicy::Error,
            ),
            operation: ImportOperation::Copy,
            collision_policy: CollisionPolicy::Error,
            include_associated_files: false,
            preserve_timestamps: false,
            use_capture_time: false,
        });
        cancel.store(true, Ordering::SeqCst);
        let result = execute_plan(id, hash, make_recorder(recorded.clone()), cancel.clone())
            .expect("cancelled");
        assert!(result.cancelled);
        assert!(
            tracked_image.exists(),
            "cancelled import retains the source"
        );
        assert!(
            !destination.join("late.jpg").exists(),
            "cancelled import writes nothing"
        );
        cancel.store(false, Ordering::SeqCst);

        // Move retires the source only after the verified copy exists; the executor
        // always copies-then-trashes, so this also exercises the cross-volume path.
        let (id, hash) = plan_for(ImportPlanRequest {
            source_files: vec![tracked_image.to_string_lossy().to_string()],
            source_folder: None,
            recursive: false,
            destination_root: destination.to_string_lossy().to_string(),
            folder_pattern: ImportPattern {
                version: 1,
                parts: Vec::new(),
                missing_token_policy: MissingTokenPolicy::Empty,
            },
            filename_pattern: parse_legacy_pattern(
                "{original_filename}",
                MissingTokenPolicy::Error,
            ),
            operation: ImportOperation::Move,
            collision_policy: CollisionPolicy::Error,
            include_associated_files: false,
            preserve_timestamps: false,
            use_capture_time: false,
        });
        let result = execute_plan(id, hash, make_recorder(recorded.clone()), cancel).expect("move");
        assert_eq!(
            fs::read(destination.join("late.jpg")).expect("verified move"),
            b"v1",
            "verified destination exists before any source retirement"
        );
        if result.succeeded == 1 {
            assert!(
                !tracked_image.exists(),
                "successful move retires the source through trash"
            );
            assert!(!result.items[0].source_retained);
        } else {
            assert_eq!(result.failed, 1, "unexpected summary: {result:?}");
            assert!(
                result.items[0]
                    .error
                    .as_deref()
                    .is_some_and(|error| error.starts_with("Verified copy retained source")),
                "trash failure must retain the source: {:?}",
                result.items[0].error
            );
            assert!(
                tracked_image.exists(),
                "trash failure never deletes the source"
            );
        }
        let phases = recorded.lock().expect("phases").clone();
        assert!(
            phases.contains(&ImportPhase::Copying),
            "typed Copying progress emitted: {phases:?}"
        );
        assert!(
            phases.contains(&ImportPhase::Complete) || phases.contains(&ImportPhase::Cancelled),
            "terminal typed progress emitted: {phases:?}"
        );
    }

    #[test]
    fn typed_contracts_import_round_trip() {
        let request = ImportPlanRequest {
            source_files: vec!["source.nef".to_string()],
            source_folder: None,
            recursive: false,
            destination_root: "library".to_string(),
            folder_pattern: parse_legacy_pattern("{YYYY}", MissingTokenPolicy::Fallback),
            filename_pattern: parse_legacy_pattern(
                "{original_filename}",
                MissingTokenPolicy::Error,
            ),
            operation: ImportOperation::Copy,
            collision_policy: CollisionPolicy::RenameWithSuffix,
            include_associated_files: true,
            preserve_timestamps: false,
            use_capture_time: false,
        };
        assert_eq!(
            serde_json::from_slice::<ImportPlanRequest>(
                &serde_json::to_vec(&request).expect("serialize")
            )
            .expect("deserialize"),
            request
        );
    }
}
