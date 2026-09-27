//! Canonical serialization and hashing.
//!
//! Canonical form: UTF-8 JSON, object keys sorted lexicographically, no
//! insignificant whitespace, serde_json number formatting (ryu). The SHA-256
//! of these bytes is the recipe content hash used for cache keys and
//! change detection.

use crate::ModelError;
use crate::types::RecipeEnvelope;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn canonical_bytes(value: &Value) -> Result<Vec<u8>, ModelError> {
    Ok(serde_json::to_vec(value)?)
}

impl RecipeEnvelope {
    /// Canonical bytes: the envelope is converted to a JSON value first so
    /// object keys are lexicographically sorted (serde_json maps are ordered)
    /// and number formatting is normalized, independent of how the document
    /// was originally ordered or formatted.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, ModelError> {
        let value = serde_json::to_value(self)?;
        canonical_bytes(&value)
    }

    /// SHA-256 (lowercase hex) of the canonical JSON bytes.
    pub fn content_hash(&self) -> Result<String, ModelError> {
        Ok(sha256_hex(&self.to_canonical_json()?))
    }
}
