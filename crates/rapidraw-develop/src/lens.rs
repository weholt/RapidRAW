//! Host-neutral lens-correction profiles and geometry warp (lap-d52 /
//! rapidraw-50c).
//!
//! Reproduces the pinned RapidRAW semantics exactly (source revision
//! `5e30bcbb246395d391ba2e9662510641ffe68e6b`,
//! `src-tauri/src/lens_correction.rs` + the lens path of
//! `image_processing.rs::warp_image_geometry` /
//! `compute_lens_auto_crop_scale`), with the engine's host boundaries:
//!
//! - **No Tauri, no app state, no bundled data.** The lensfun XML database is
//!   parsed from bytes the host supplies. This engine bundles no lens data;
//!   distribution rights for the upstream lensfun snapshot are unresolved
//!   (Lap `docs/raw-development/provenance.json`, release hold) and hosts
//!   must record profile provenance before attaching it to a recipe.
//! - **Versioned profile identity.** Hosts persist
//!   [`rapidraw_edit_model::LensProfileRef`] (maker/model/version/sha256)
//!   alongside the resolved [`rapidraw_edit_model::LensDistortionParams`];
//!   renders verify the profile resource is still present and unchanged.
//! - **Explicit capability errors (spec A7).** One deliberate tightening of
//!   the reference: a lens whose *selected* distortion calibration uses an
//!   unsupported model is [`LensError::UnsupportedDistortionModel`] instead of
//!   the reference's silent zeroing — silently enabling a profile that cannot
//!   correct distortion would change exports invisibly. An unknown lens is
//!   [`LensError::ProfileNotFound`]. Lenses with calibration but no
//!   distortion coefficients, and non-linear TCA models (the reference only
//!   applies the linear `vr`/`vb` term), resolve neutrally like the reference
//!   and carry a visible [`LensCapabilityNotice`].
//! - **Warp parity.** [`lens_warp`] applies radial lens distortion (poly3 /
//!   poly5 and ptlens), lateral chromatic aberration, vignetting correction
//!   and the auto-crop scale with the reference's exact numeric pipeline
//!   (f32 coordinates, f64 radial math, truncating bilinear edge handling).
//!   The perspective-transform matrix terms (vertical/horizontal/rotate/
//!   aspect/scale/offset) are a separate geometry increment and stay
//!   identity here; the manual `transformDistortion` slider term *is*
//!   included, exactly as the reference's lens path applies it. The
//!   reference's interactive-preview vignette damping (`*0.4` / `*0.8` on a
//!   tonemapped preview base) is host policy, not engine behavior: hosts
//!   render the warp on linear data at the export-path amounts.

use fuzzy_matcher::FuzzyMatcher;
use serde::Deserialize;
use std::cmp::Ordering;

use rayon::prelude::*;

use rapidraw_edit_model::{LensDistortionParams, Recipe};

use crate::buffer::LinearImage;

// ---------------------------------------------------------------------------
// Errors and capability notices
// ---------------------------------------------------------------------------

/// Typed lens-profile failure. Every outcome is explicit; hosts surface these
/// instead of silently rendering uncorrected (or differently corrected) pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LensError {
    /// The profile document does not parse as lensfun XML.
    Parse { detail: String },
    /// No lens matches the requested maker/model in the supplied database.
    ProfileNotFound { maker: String, model: String },
    /// The selected distortion calibration uses a model this engine does not
    /// implement (`ptbrown`, `aicom`, ...). The reference silently zeroed
    /// these; that would silently change exports (spec A7), so it is an
    /// explicit error here.
    UnsupportedDistortionModel {
        maker: String,
        model: String,
        distortion_model: String,
        detail: String,
    },
}

impl std::fmt::Display for LensError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LensError::Parse { detail } => write!(f, "lens profile XML is invalid: {detail}"),
            LensError::ProfileNotFound { maker, model } => {
                write!(
                    f,
                    "no lens profile found for maker '{maker}' model '{model}'"
                )
            }
            LensError::UnsupportedDistortionModel {
                maker,
                model,
                distortion_model,
                detail,
            } => write!(
                f,
                "unsupported lens distortion model '{distortion_model}' for {maker} {model}: {detail}"
            ),
        }
    }
}

impl std::error::Error for LensError {}

/// A visible, non-fatal capability notice attached to a resolved lens. The
/// correction output matches the pinned reference; the notice keeps the
/// limitation visible instead of implicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensCapabilityNotice {
    /// Stable machine string, e.g. `no-calibration-data`,
    /// `unsupported-tca-model`.
    pub kind: &'static str,
    pub detail: String,
}

// ---------------------------------------------------------------------------
// Lensfun XML model (pinned reference structs)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Distortion {
    #[serde(rename = "@model")]
    pub model: String,
    #[serde(rename = "@focal")]
    pub focal: f32,
    #[serde(rename = "@real-focal")]
    pub real_focal: Option<f32>,
    #[serde(rename = "@k1")]
    pub k1: Option<f32>,
    #[serde(rename = "@k2")]
    pub k2: Option<f32>,
    #[serde(rename = "@k3")]
    pub k3: Option<f32>,
    #[serde(rename = "@a")]
    pub a: Option<f32>,
    #[serde(rename = "@b")]
    pub b: Option<f32>,
    #[serde(rename = "@c")]
    pub c: Option<f32>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Tca {
    #[serde(rename = "@model")]
    pub model: String,
    #[serde(rename = "@focal")]
    pub focal: f32,
    #[serde(rename = "@vr")]
    pub vr: Option<f32>,
    #[serde(rename = "@vb")]
    pub vb: Option<f32>,
    #[serde(rename = "@cr")]
    pub cr: Option<f32>,
    #[serde(rename = "@cb")]
    pub cb: Option<f32>,
    #[serde(rename = "@br")]
    pub br: Option<f32>,
    #[serde(rename = "@bb")]
    pub bb: Option<f32>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Vignetting {
    #[serde(rename = "@model")]
    pub model: String,
    #[serde(rename = "@focal")]
    pub focal: f32,
    #[serde(rename = "@aperture")]
    pub aperture: f32,
    #[serde(rename = "@distance")]
    pub distance: Option<f32>,
    #[serde(rename = "@k1")]
    pub k1: Option<f32>,
    #[serde(rename = "@k2")]
    pub k2: Option<f32>,
    #[serde(rename = "@k3")]
    pub k3: Option<f32>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum CalibrationElement {
    Distortion(Distortion),
    Tca(Tca),
    Vignetting(Vignetting),
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Calibration {
    #[serde(rename = "$value", default)]
    pub elements: Vec<CalibrationElement>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Focal {
    #[serde(rename = "@value")]
    pub value: Option<f32>,
    #[serde(rename = "@min")]
    pub min: Option<f32>,
    #[serde(rename = "@max")]
    pub max: Option<f32>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Aperture {
    #[serde(rename = "@min")]
    pub min: Option<f32>,
    #[serde(rename = "@max")]
    pub max: Option<f32>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct Lens {
    #[serde(default)]
    pub maker: Vec<MultiName>,
    #[serde(default)]
    pub model: Vec<MultiName>,
    #[serde(default)]
    pub mount: Vec<String>,
    pub cropfactor: Option<f32>,
    pub calibration: Option<Calibration>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub focal: Option<Focal>,
    pub aspect_ratio: Option<String>,
    pub center: Option<String>,
    pub compat: Option<String>,
    pub notes: Option<String>,
    pub aperture: Option<Aperture>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct Camera {
    pub maker: Vec<MultiName>,
    pub model: Vec<MultiName>,
    pub mount: String,
    pub cropfactor: f32,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Default)]
pub struct LensDatabase {
    #[serde(rename = "camera", default)]
    pub cameras: Vec<Camera>,
    #[serde(rename = "lens", default)]
    pub lenses: Vec<Lens>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct MultiName {
    #[serde(rename = "@lang")]
    pub lang: Option<String>,
    #[serde(rename = "$value")]
    pub value: String,
}

/// Parses a lensfun XML document (one database file). Hosts hash the exact
/// bytes and persist the hash as the profile identity.
pub fn parse_lensfun_db(xml: &str) -> Result<LensDatabase, LensError> {
    quick_xml::de::from_str::<LensDatabase>(xml).map_err(|err| LensError::Parse {
        detail: err.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Names and matching (reference `Lens` helpers + `find_best_lens_match`)
// ---------------------------------------------------------------------------

fn strip_maker_prefix(name: &str, maker: &str) -> String {
    if name.to_lowercase().starts_with(&maker.to_lowercase())
        && let Some(rest) = name.get(maker.len()..)
    {
        let trimmed = rest.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    name.to_string()
}

impl Lens {
    pub fn get_full_model_name(&self) -> String {
        self.model
            .iter()
            .find(|m| m.lang.as_deref() == Some("en"))
            .or_else(|| self.model.first())
            .map(|m| m.value.clone())
            .unwrap_or_else(|| "Unknown Model".to_string())
    }

    pub fn get_canonical_model_name(&self) -> String {
        self.model
            .iter()
            .find(|m| m.lang.is_none())
            .or_else(|| self.model.first())
            .map(|m| m.value.clone())
            .unwrap_or_else(|| "Unknown Model".to_string())
    }

    pub fn get_name(&self) -> String {
        let raw_name = self.get_full_model_name();
        let maker = self.get_maker();

        if raw_name.to_lowercase().starts_with(&maker.to_lowercase())
            && let Some(rest) = raw_name.get(maker.len()..)
        {
            let stripped = rest.trim();
            if !stripped.is_empty() {
                return stripped.to_string();
            }
        }

        raw_name
    }

    pub fn get_maker(&self) -> String {
        self.maker
            .iter()
            .find(|m| m.lang.as_deref() == Some("en"))
            .or_else(|| self.maker.first())
            .map(|m| m.value.clone())
            .unwrap_or_else(|| "Misc".to_string())
    }

    pub fn get_display_name(&self, all_maker_lenses: &[&Lens]) -> String {
        let my_short = self.get_name();
        let short_count = all_maker_lenses
            .iter()
            .filter(|l| l.get_name() == my_short)
            .count();

        if short_count <= 1 {
            return my_short;
        }

        let maker = self.get_maker();
        let my_canonical_short = strip_maker_prefix(&self.get_canonical_model_name(), &maker);

        let canonical_short_count = all_maker_lenses
            .iter()
            .filter(|l| {
                strip_maker_prefix(&l.get_canonical_model_name(), &l.get_maker())
                    == my_canonical_short
            })
            .count();

        if canonical_short_count <= 1 {
            return my_canonical_short;
        }

        let my_canonical = self.get_canonical_model_name();
        let canonical_count = all_maker_lenses
            .iter()
            .filter(|l| l.get_canonical_model_name() == my_canonical)
            .count();

        if canonical_count <= 1 {
            return my_canonical;
        }

        if let Some(cf) = self.cropfactor {
            format!("{} (crop {:.1}x)", my_canonical_short, cf)
        } else {
            my_canonical_short
        }
    }
}

pub fn lenses_for_maker<'a>(db: &'a LensDatabase, maker: &str) -> Vec<&'a Lens> {
    db.lenses
        .iter()
        .filter(|l| l.get_maker() == maker)
        .collect()
}

/// Reference `find_best_lens_match`: fuzzy-match a camera-reported maker and
/// model against the database, preferring the maker's own lenses, then any
/// lens. Returns `(maker, display_name)`.
pub fn find_best_lens_match(
    db: &LensDatabase,
    maker: &str,
    model: &str,
) -> Option<(String, String)> {
    let clean_maker = maker.trim().trim_matches('"').to_string();
    let clean_model = model.trim().trim_matches('"').to_string();
    let matcher = fuzzy_matcher::skim::SkimMatcherV2::default().ignore_case();

    let lenses_from_maker: Vec<&Lens> = db
        .lenses
        .iter()
        .filter(|lens| lens.get_maker().eq_ignore_ascii_case(&clean_maker))
        .collect();

    if !lenses_from_maker.is_empty() {
        let best_match = lenses_from_maker
            .iter()
            .filter_map(|lens| {
                let english_name = lens.get_full_model_name();
                let canonical_name = lens.get_canonical_model_name();

                let score_english = matcher
                    .fuzzy_match(&english_name, &clean_model)
                    .unwrap_or(0);
                let score_canonical = matcher
                    .fuzzy_match(&canonical_name, &clean_model)
                    .unwrap_or(0);
                let score = score_english.max(score_canonical);

                if score > 0 {
                    let best_name = if score_canonical > score_english {
                        &canonical_name
                    } else {
                        &english_name
                    };
                    let length_penalty =
                        (best_name.len() as i64 - clean_model.len() as i64).max(0) / 2;
                    let adjusted_score = score - length_penalty;
                    Some((adjusted_score, *lens))
                } else {
                    None
                }
            })
            .max_by_key(|(score, _)| *score);

        if let Some((_, best_lens)) = best_match {
            return Some((
                best_lens.get_maker(),
                best_lens.get_display_name(&lenses_from_maker),
            ));
        }
    }

    let best_match_fallback = db
        .lenses
        .iter()
        .filter_map(|lens| {
            let english_name = lens.get_full_model_name();
            let canonical_name = lens.get_canonical_model_name();

            let score_english = matcher
                .fuzzy_match(&english_name, &clean_model)
                .unwrap_or(0);
            let score_canonical = matcher
                .fuzzy_match(&canonical_name, &clean_model)
                .unwrap_or(0);
            let score = score_english.max(score_canonical);

            if score > 0 { Some((score, lens)) } else { None }
        })
        .max_by_key(|(score, _): &(i64, _)| *score);

    if let Some((_, best_lens)) = best_match_fallback {
        let lens_maker = best_lens.get_maker();
        let maker_lenses = lenses_for_maker(db, &lens_maker);
        return Some((lens_maker, best_lens.get_display_name(&maker_lenses)));
    }

    None
}

// ---------------------------------------------------------------------------
// Parameter resolution (reference `get_distortion_params`)
// ---------------------------------------------------------------------------

fn extract_dist_params(dist: &Distortion) -> Result<(f64, f64, f64, u32), String> {
    match dist.model.as_str() {
        "poly3" | "poly5" => Ok((
            dist.k1.unwrap_or(0.0) as f64,
            dist.k2.unwrap_or(0.0) as f64,
            dist.k3.unwrap_or(0.0) as f64,
            0,
        )),
        "ptlens" => Ok((
            dist.a.unwrap_or(0.0) as f64,
            dist.b.unwrap_or(0.0) as f64,
            dist.c.unwrap_or(0.0) as f64,
            1,
        )),
        other => Err(other.to_string()),
    }
}

fn extract_tca_params(tca: &Tca) -> (f64, f64) {
    (tca.vr.unwrap_or(1.0) as f64, tca.vb.unwrap_or(1.0) as f64)
}

fn extract_vig_params(vig: &Vignetting) -> (f64, f64, f64) {
    (
        vig.k1.unwrap_or(0.0) as f64,
        vig.k2.unwrap_or(0.0) as f64,
        vig.k3.unwrap_or(0.0) as f64,
    )
}

impl Lens {
    /// Reference `get_distortion_params`: focal interpolation for distortion
    /// and TCA, aperture-then-distance selection for vignetting. The
    /// unsupported-model case is an error (see [`LensError`]).
    pub fn get_distortion_params(
        &self,
        focal_length: f32,
        aperture: Option<f32>,
        distance: Option<f32>,
    ) -> Result<Option<(LensDistortionParams, Vec<LensCapabilityNotice>)>, LensError> {
        let Some(cal) = self.calibration.as_ref() else {
            return Ok(None);
        };
        let maker = self.get_maker();
        let model = self.get_canonical_model_name();
        let mut notices: Vec<LensCapabilityNotice> = Vec::new();

        let mut distortions: Vec<&Distortion> = cal
            .elements
            .iter()
            .filter_map(|e| {
                if let CalibrationElement::Distortion(d) = e {
                    Some(d)
                } else {
                    None
                }
            })
            .collect();

        let mut tcas: Vec<&Tca> = cal
            .elements
            .iter()
            .filter_map(|e| {
                if let CalibrationElement::Tca(t) = e {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();

        let mut vignettings: Vec<&Vignetting> = cal
            .elements
            .iter()
            .filter_map(|e| {
                if let CalibrationElement::Vignetting(v) = e {
                    Some(v)
                } else {
                    None
                }
            })
            .collect();

        let (k1, k2, k3, model_code) = if distortions.is_empty() {
            notices.push(LensCapabilityNotice {
                kind: "no-distortion-data",
                detail: format!(
                    "profile for {maker} {model} carries no distortion calibration; distortion correction stays neutral"
                ),
                });
            (0.0, 0.0, 0.0, 0)
        } else {
            distortions.sort_by(|a, b| a.focal.partial_cmp(&b.focal).unwrap_or(Ordering::Equal));

            if let Some(exact) = distortions
                .iter()
                .find(|d| (d.focal - focal_length).abs() < 1e-5)
            {
                extract_dist_params_checked(exact, &maker, &model)?
            } else if focal_length < distortions[0].focal {
                extract_dist_params_checked(distortions[0], &maker, &model)?
            } else if focal_length > distortions.last().unwrap().focal {
                extract_dist_params_checked(distortions.last().unwrap(), &maker, &model)?
            } else {
                let mut res = (0.0, 0.0, 0.0, 0);
                for pair in distortions.windows(2) {
                    let (d1, d2) = (&pair[0], &pair[1]);

                    if focal_length >= d1.focal && focal_length <= d2.focal {
                        let p1 = extract_dist_params_checked(d1, &maker, &model)?;
                        let p2 = extract_dist_params_checked(d2, &maker, &model)?;

                        let range = d2.focal - d1.focal;
                        if range.abs() < 1e-5 || p1.3 != p2.3 {
                            res = p1;
                        } else {
                            let t = (focal_length - d1.focal) / range;
                            res = (
                                p1.0 + t as f64 * (p2.0 - p1.0),
                                p1.1 + t as f64 * (p2.1 - p1.1),
                                p1.2 + t as f64 * (p2.2 - p1.2),
                                p1.3,
                            );
                        }
                        break;
                    }
                }
                res
            }
        };

        let (tca_vr, tca_vb) = if tcas.is_empty() {
            (1.0, 1.0)
        } else {
            tcas.sort_by(|a, b| a.focal.partial_cmp(&b.focal).unwrap_or(Ordering::Equal));

            if let Some(exact) = tcas.iter().find(|d| (d.focal - focal_length).abs() < 1e-5) {
                extract_tca_params(exact)
            } else if focal_length < tcas[0].focal {
                extract_tca_params(tcas[0])
            } else if focal_length > tcas.last().unwrap().focal {
                extract_tca_params(tcas.last().unwrap())
            } else {
                let mut res = (1.0, 1.0);
                for pair in tcas.windows(2) {
                    let (d1, d2) = (&pair[0], &pair[1]);
                    if focal_length >= d1.focal && focal_length <= d2.focal {
                        let p1 = extract_tca_params(d1);
                        let p2 = extract_tca_params(d2);

                        let range = d2.focal - d1.focal;
                        if range.abs() < 1e-5 {
                            res = p1;
                        } else {
                            let t = (focal_length - d1.focal) / range;
                            res = (
                                p1.0 + t as f64 * (p2.0 - p1.0),
                                p1.1 + t as f64 * (p2.1 - p1.1),
                            );
                        }
                        break;
                    }
                }
                res
            }
        };

        // The reference applies only the linear vr/vb TCA term, for any TCA
        // model that carries it. poly3 entries carry vr/vb plus cubic
        // coefficients (cr/cb/br/bb) the reference ignores; models without
        // vr/vb resolve to neutral. Keep that output, but surface the
        // partial/missing support instead of leaving it implicit.
        let tca_models: Vec<&str> = {
            let mut seen: Vec<&str> = Vec::new();
            for t in &tcas {
                if !seen.contains(&t.model.as_str()) {
                    seen.push(t.model.as_str());
                }
            }
            seen
        };
        let unsupported_tca: Vec<&str> = tca_models
            .iter()
            .copied()
            .filter(|m| !m.is_empty() && *m != "linear" && *m != "poly3")
            .collect();
        if !unsupported_tca.is_empty() {
            notices.push(LensCapabilityNotice {
                kind: "unsupported-tca-model",
                detail: format!(
                    "profile for {maker} {model} uses TCA model(s) [{}] without a supported vr/vb linear term; TCA correction stays neutral",
                    unsupported_tca.join(", ")
                ),
            });
        } else if tca_models.contains(&"poly3") {
            notices.push(LensCapabilityNotice {
                kind: "partial-tca-model",
                detail: format!(
                    "profile for {maker} {model} uses poly3 TCA calibration; only its linear vr/vb term is applied, the cubic cr/cb/br/bb coefficients are ignored"
                ),
            });
        }

        let (vig_k1, vig_k2, vig_k3) = if vignettings.is_empty() {
            (0.0, 0.0, 0.0)
        } else {
            let target_aperture = aperture.unwrap_or(3.5);
            let target_distance = distance.unwrap_or(1000.0);

            vignettings.sort_by(|a, b| a.focal.partial_cmp(&b.focal).unwrap_or(Ordering::Equal));

            let find_best_vig = |items: &[&Vignetting]| -> (f64, f64, f64) {
                let best_aperture_item = items.iter().min_by(|a, b| {
                    (a.aperture - target_aperture)
                        .abs()
                        .partial_cmp(&(b.aperture - target_aperture).abs())
                        .unwrap_or(Ordering::Equal)
                });
                if let Some(best_ap) = best_aperture_item {
                    let candidates: Vec<&&Vignetting> = items
                        .iter()
                        .filter(|x| (x.aperture - best_ap.aperture).abs() < 0.01)
                        .collect();
                    let best_dist = candidates.into_iter().min_by(|a, b| {
                        let da = a.distance.unwrap_or(1000.0);
                        let db = b.distance.unwrap_or(1000.0);
                        (da - target_distance)
                            .abs()
                            .partial_cmp(&(db - target_distance).abs())
                            .unwrap_or(Ordering::Equal)
                    });
                    extract_vig_params(best_dist.unwrap_or(best_ap))
                } else {
                    (0.0, 0.0, 0.0)
                }
            };

            if focal_length <= vignettings[0].focal + 0.01 {
                let group: Vec<&Vignetting> = vignettings
                    .iter()
                    .filter(|x| (x.focal - vignettings[0].focal).abs() < 0.01)
                    .copied()
                    .collect();
                find_best_vig(&group)
            } else if focal_length >= vignettings.last().unwrap().focal - 0.01 {
                let last_focal = vignettings.last().unwrap().focal;
                let group: Vec<&Vignetting> = vignettings
                    .iter()
                    .filter(|x| (x.focal - last_focal).abs() < 0.01)
                    .copied()
                    .collect();
                find_best_vig(&group)
            } else {
                let mut res = (0.0, 0.0, 0.0);
                let unique_focals: Vec<f32> = {
                    let mut f: Vec<f32> = vignettings.iter().map(|v| v.focal).collect();
                    f.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
                    f.dedup_by(|a, b| (*a - *b).abs() < 0.01);
                    f
                };
                for pair in unique_focals.windows(2) {
                    let (f1, f2) = (pair[0], pair[1]);
                    if focal_length >= f1 && focal_length <= f2 {
                        let group1: Vec<&Vignetting> = vignettings
                            .iter()
                            .filter(|x| (x.focal - f1).abs() < 0.01)
                            .copied()
                            .collect();
                        let group2: Vec<&Vignetting> = vignettings
                            .iter()
                            .filter(|x| (x.focal - f2).abs() < 0.01)
                            .copied()
                            .collect();

                        let p1 = find_best_vig(&group1);
                        let p2 = find_best_vig(&group2);

                        let range = f2 - f1;
                        if range.abs() > 0.01 {
                            let t = (focal_length - f1) / range;
                            res = (
                                p1.0 + t as f64 * (p2.0 - p1.0),
                                p1.1 + t as f64 * (p2.1 - p1.1),
                                p1.2 + t as f64 * (p2.2 - p1.2),
                            );
                        } else {
                            res = p1;
                        }
                        break;
                    }
                }
                res
            }
        };

        Ok(Some((
            LensDistortionParams {
                k1,
                k2,
                k3,
                model: f64::from(model_code),
                tca_vr,
                tca_vb,
                vig_k1,
                vig_k2,
                vig_k3,
            },
            notices,
        )))
    }
}

/// Like the reference `extract_dist_params`, but unsupported models are
/// explicit capability errors instead of silent zeros (spec A7).
fn extract_dist_params_checked(
    dist: &Distortion,
    maker: &str,
    model: &str,
) -> Result<(f64, f64, f64, u32), LensError> {
    extract_dist_params(dist).map_err(|distortion_model| LensError::UnsupportedDistortionModel {
        maker: maker.to_string(),
        model: model.to_string(),
        distortion_model: distortion_model.clone(),
        detail: format!(
            "the selected calibration element at focal {} uses model '{distortion_model}'; supported models are poly3, poly5 and ptlens",
            dist.focal
        ),
    })
}

/// Reference `resolve_lens_params`, made total: the outcome is either typed
/// data or a typed error — never a silent `None` for a requested lens.
pub fn resolve_lens_params(
    db: &LensDatabase,
    maker: &str,
    model: &str,
    focal_length: f32,
    aperture: Option<f32>,
    distance: Option<f32>,
) -> Result<(LensDistortionParams, Vec<LensCapabilityNotice>), LensError> {
    let maker_lenses = lenses_for_maker(db, maker);
    let lens = maker_lenses
        .iter()
        .find(|l| l.get_display_name(&maker_lenses) == model)
        .ok_or_else(|| LensError::ProfileNotFound {
            maker: maker.to_string(),
            model: model.to_string(),
        })?;
    match lens.get_distortion_params(focal_length, aperture, distance)? {
        Some(resolved) => Ok(resolved),
        None => {
            // A selected lens with no calibration block: reference returns
            // None and the frontend leaves the coefficients unset (neutral).
            Ok((
                LensDistortionParams {
                    k1: 0.0,
                    k2: 0.0,
                    k3: 0.0,
                    model: 0.0,
                    tca_vr: 1.0,
                    tca_vb: 1.0,
                    vig_k1: 0.0,
                    vig_k2: 0.0,
                    vig_k3: 0.0,
                },
                vec![LensCapabilityNotice {
                    kind: "no-calibration-data",
                    detail: format!(
                        "profile for {maker} {model} carries no calibration block; all corrections stay neutral"
                    ),
                }],
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Lens geometry warp (reference warp path, identity perspective transform)
// ---------------------------------------------------------------------------

/// Effective lens-correction warp inputs, in the reference's numeric shapes
/// (amounts normalized to 1.0 = nominal, coefficients already truncated
/// through f32 exactly like the reference's `GeometryParams`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LensWarpParams {
    /// Manual `transformDistortion` slider term (raw recipe units); the
    /// reference's lens warp applies it together with the profile terms.
    pub distortion: f64,
    pub lens_dist_k1: f64,
    pub lens_dist_k2: f64,
    pub lens_dist_k3: f64,
    /// 0 = poly3/poly5, 1 = ptlens.
    pub lens_model: u32,
    pub lens_distortion_amount: f64,
    pub lens_distortion_enabled: bool,
    pub tca_vr: f64,
    pub tca_vb: f64,
    pub lens_tca_amount: f64,
    pub lens_tca_enabled: bool,
    pub vig_k1: f64,
    pub vig_k2: f64,
    pub vig_k3: f64,
    pub lens_vignette_amount: f64,
    pub lens_vignette_enabled: bool,
}

impl Default for LensWarpParams {
    fn default() -> Self {
        Self {
            distortion: 0.0,
            lens_dist_k1: 0.0,
            lens_dist_k2: 0.0,
            lens_dist_k3: 0.0,
            lens_model: 0,
            lens_distortion_amount: 1.0,
            lens_distortion_enabled: true,
            tca_vr: 1.0,
            tca_vb: 1.0,
            lens_tca_amount: 1.0,
            lens_tca_enabled: true,
            vig_k1: 0.0,
            vig_k2: 0.0,
            vig_k3: 0.0,
            lens_vignette_amount: 1.0,
            lens_vignette_enabled: true,
        }
    }
}

impl LensWarpParams {
    /// Builds the warp inputs from a recipe, reproducing the reference's
    /// f32 truncation of every coefficient and amount.
    pub fn from_recipe(recipe: &Recipe) -> Self {
        let params = recipe.lens_distortion_params;
        Self {
            distortion: recipe.transform_distortion as f32 as f64,
            lens_dist_k1: params.map(|p| p.k1 as f32 as f64).unwrap_or(0.0),
            lens_dist_k2: params.map(|p| p.k2 as f32 as f64).unwrap_or(0.0),
            lens_dist_k3: params.map(|p| p.k3 as f32 as f64).unwrap_or(0.0),
            lens_model: params.map(|p| p.model as u32).unwrap_or(0),
            lens_distortion_amount: (recipe.lens_distortion_amount as f32) as f64 / 100.0,
            lens_distortion_enabled: recipe.lens_distortion_enabled,
            tca_vr: params.map(|p| p.tca_vr as f32 as f64).unwrap_or(1.0),
            tca_vb: params.map(|p| p.tca_vb as f32 as f64).unwrap_or(1.0),
            lens_tca_amount: (recipe.lens_tca_amount as f32) as f64 / 100.0,
            lens_tca_enabled: recipe.lens_tca_enabled,
            vig_k1: params.map(|p| p.vig_k1 as f32 as f64).unwrap_or(0.0),
            vig_k2: params.map(|p| p.vig_k2 as f32 as f64).unwrap_or(0.0),
            vig_k3: params.map(|p| p.vig_k3 as f32 as f64).unwrap_or(0.0),
            lens_vignette_amount: (recipe.lens_vignette_amount as f32) as f64 / 100.0,
            lens_vignette_enabled: recipe.lens_vignette_enabled,
        }
    }

    /// Reference `is_geometry_identity`, lens terms only.
    pub fn is_identity(&self) -> bool {
        let dist_identity = !self.lens_distortion_enabled
            || ((self.lens_distortion_amount - 1.0).abs() < 1e-4
                && self.lens_dist_k1.abs() < 1e-6
                && self.lens_dist_k2.abs() < 1e-6
                && self.lens_dist_k3.abs() < 1e-6);

        let tca_identity = !self.lens_tca_enabled
            || ((self.lens_tca_amount - 1.0).abs() < 1e-4
                && (self.tca_vr - 1.0).abs() < 1e-6
                && (self.tca_vb - 1.0).abs() < 1e-6);

        let vig_identity = !self.lens_vignette_enabled
            || ((self.lens_vignette_amount - 1.0).abs() < 1e-4
                && self.vig_k1.abs() < 1e-6
                && self.vig_k2.abs() < 1e-6
                && self.vig_k3.abs() < 1e-6);

        self.distortion == 0.0 && dist_identity && tca_identity && vig_identity
    }
}

/// Reference `compute_lens_auto_crop_scale` (f64 pipeline over 8 border
/// sample points). `LensWarpParams` values already carry the reference's
/// f32 truncation.
pub fn lens_auto_crop_scale(width: u32, height: u32, params: &LensWarpParams) -> f64 {
    let cx = (f64::from(width)) / 2.0;
    let cy = (f64::from(height)) / 2.0;
    let half_diagonal = (cx * cx + cy * cy).sqrt();
    let max_radius_sq_inv = 1.0 / (cx * cx + cy * cy);

    let lk1 = params.lens_dist_k1;
    let lk2 = params.lens_dist_k2;
    let lk3 = params.lens_dist_k3;
    let lens_dist_amt = params.lens_distortion_amount * 2.5;

    let k_distortion = (params.distortion / 100.0) * 2.5;

    let has_lens_correction = params.lens_distortion_enabled
        && (lk1.abs() > 1e-6 || lk2.abs() > 1e-6 || lk3.abs() > 1e-6);
    let is_ptlens = params.lens_model == 1;

    let sample_points: [(f64, f64); 8] = [
        (cx, 0.0),
        (cx, f64::from(height)),
        (0.0, cy),
        (f64::from(width), cy),
        (0.0, 0.0),
        (f64::from(width), 0.0),
        (0.0, f64::from(height)),
        (f64::from(width), f64::from(height)),
    ];

    let mut max_scale: f64 = 1.0;

    for &(px, py) in &sample_points {
        let dx = px - cx;
        let dy = py - cy;
        let ru = (dx * dx + dy * dy).sqrt();
        if ru < 1e-6 {
            continue;
        }

        let mut mapped_dx = dx;
        let mut mapped_dy = dy;

        if has_lens_correction {
            let ru_norm = ru / half_diagonal;
            let ru_norm2 = ru_norm * ru_norm;

            let rd_norm = if is_ptlens {
                let a = lk1;
                let b = lk2;
                let c = lk3;
                let d = 1.0 - a - b - c;
                ru_norm * (a * ru_norm2 * ru_norm + b * ru_norm2 + c * ru_norm + d)
            } else {
                ru_norm
                    * (1.0
                        + lk1 * ru_norm2
                        + lk2 * (ru_norm2 * ru_norm2)
                        + lk3 * (ru_norm2 * ru_norm2 * ru_norm2))
            };

            let effective_r_norm = ru_norm + (rd_norm - ru_norm) * lens_dist_amt;
            let scale = effective_r_norm / ru_norm;

            mapped_dx *= scale;
            mapped_dy *= scale;
        }

        if k_distortion.abs() > 1e-5 {
            let r2_norm = (mapped_dx * mapped_dx + mapped_dy * mapped_dy) * max_radius_sq_inv;
            let f = 1.0 + k_distortion * r2_norm;
            mapped_dx *= f;
            mapped_dy *= f;
        }

        let mapped_ru = (mapped_dx * mapped_dx + mapped_dy * mapped_dy).sqrt();
        let scale = mapped_ru / ru;

        if scale > max_scale {
            max_scale = scale;
        }
    }

    if max_scale > 1.0 {
        max_scale * 1.002
    } else {
        max_scale
    }
}

/// Reference `interpolate_pixel`: clamped-none bilinear; outside
/// `[0, w-1] x [0, h-1]` (or NaN coordinates) the pixel stays untouched.
#[inline(always)]
fn interpolate_pixel(
    src: &LinearImage,
    src_width: usize,
    src_height: usize,
    x: f32,
    y: f32,
    pixel_out: &mut [f32; 3],
) {
    if x.is_nan()
        || y.is_nan()
        || x < 0.0
        || y < 0.0
        || x >= (src_width as f32 - 1.0)
        || y >= (src_height as f32 - 1.0)
    {
        return;
    }

    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;

    let wx = x - x0 as f32;
    let wy = y - y0 as f32;
    let one_minus_wx = 1.0 - wx;
    let one_minus_wy = 1.0 - wy;

    let stride = src_width * 3;
    let idx_row0 = y0 * stride;
    let idx_row1 = idx_row0 + stride;
    let idx_p00 = idx_row0 + x0 * 3;
    let raw = src.rgb();

    for c in 0..3 {
        let p00 = raw[idx_p00 + c];
        let p10 = raw[idx_p00 + 3 + c];
        let p01 = raw[idx_row1 + x0 * 3 + c];
        let p11 = raw[idx_row1 + x0 * 3 + 3 + c];

        let top_r = p00 * one_minus_wx + p10 * wx;
        let bot_r = p01 * one_minus_wx + p11 * wx;
        pixel_out[c] = top_r * one_minus_wy + bot_r * wy;
    }
}

/// Reference `TcaContext`: shared per-render state for TCA sampling.
struct TcaContext<'a> {
    src: &'a LinearImage,
    src_width: usize,
    src_height: usize,
    cx: f32,
    cy: f32,
}

/// Reference `interpolate_pixel_with_tca`: per-channel bilinear with the
/// red/blue channel centers scaled by `vr`/`vb` around the image center;
/// coordinates are clamped instead of rejected.
#[inline(always)]
fn interpolate_pixel_with_tca(
    tca: &TcaContext,
    base_x: f32,
    base_y: f32,
    vr: f32,
    vb: f32,
    pixel_out: &mut [f32; 3],
) {
    let src = tca.src;
    let src_width = tca.src_width;
    let src_height = tca.src_height;
    let cx = tca.cx;
    let cy = tca.cy;
    let gx = base_x;
    let gy = base_y;

    let rx = cx + (base_x - cx) * vr;
    let ry = cy + (base_y - cy) * vr;

    let bx = cx + (base_x - cx) * vb;
    let by = cy + (base_y - cy) * vb;

    let sample_channel = |target_x: f32, target_y: f32, channel_idx: usize| -> f32 {
        if target_x.is_nan() || target_y.is_nan() {
            return 0.0;
        }

        let x_clamped = target_x.clamp(0.0, src_width as f32 - 1.0);
        let y_clamped = target_y.clamp(0.0, src_height as f32 - 1.0);

        let mut x0 = x_clamped.floor() as usize;
        let mut y0 = y_clamped.floor() as usize;

        if x0 >= src_width - 1 {
            x0 = src_width.saturating_sub(2);
        }
        if y0 >= src_height - 1 {
            y0 = src_height.saturating_sub(2);
        }

        let wx = x_clamped - x0 as f32;
        let wy = y_clamped - y0 as f32;
        let one_minus_wx = 1.0 - wx;
        let one_minus_wy = 1.0 - wy;

        let stride = src_width * 3;
        let idx_row0 = y0 * stride;
        let idx_row1 = idx_row0 + stride;

        let idx_p00 = idx_row0 + x0 * 3 + channel_idx;
        let raw = src.rgb();

        let p00 = raw[idx_p00];
        let p10 = raw[idx_p00 + 3];
        let p01 = raw[idx_row1 + x0 * 3 + channel_idx];
        let p11 = raw[idx_row1 + x0 * 3 + 3 + channel_idx];

        let top = p00 * one_minus_wx + p10 * wx;
        let bot = p01 * one_minus_wx + p11 * wx;
        top * one_minus_wy + bot * wy
    };

    pixel_out[0] = sample_channel(rx, ry, 0);
    pixel_out[1] = sample_channel(gx, gy, 1);
    pixel_out[2] = sample_channel(bx, by, 2);
}

/// Lens-correction geometry warp on a linear image, reproducing the pinned
/// reference `warp_image_geometry` exactly for the identity perspective
/// transform this engine applies: per output pixel, un-zoom by the auto-crop
/// scale, apply the radial lens distortion (poly3/poly5 or ptlens) and the
/// manual distortion term, bilinearly sample (TCA per channel when enabled),
/// then apply the vignetting correction gain.
pub fn lens_warp(image: &LinearImage, params: &LensWarpParams) -> LinearImage {
    if params.is_identity() {
        return image.clone();
    }

    let (width, height) = image.dimensions();
    let (width_usize, height_usize) = (width as usize, height as usize);

    // Reference `build_transform_matrices` at identity: cx = width/2,
    // cy = height/2, inverse transform = identity, half diagonal as f64.
    let cx = width as f32 / 2.0;
    let cy = height as f32 / 2.0;
    let half_diagonal =
        (f64::from(width) * f64::from(width) + f64::from(height) * f64::from(height)).sqrt() / 2.0;

    let max_radius_sq_inv = 1.0 / ((cx * cx + cy * cy) as f64);
    let hd = half_diagonal;

    let k_distortion = (params.distortion / 100.0) * 2.5;
    let lk1 = params.lens_dist_k1;
    let lk2 = params.lens_dist_k2;
    let lk3 = params.lens_dist_k3;
    let lens_dist_amt = params.lens_distortion_amount * 2.5;

    let has_lens_correction = params.lens_distortion_enabled
        && (lk1.abs() > 1e-6 || lk2.abs() > 1e-6 || lk3.abs() > 1e-6);
    let is_ptlens = params.lens_model == 1;

    let auto_crop_scale = if has_lens_correction || k_distortion.abs() > 1e-5 {
        lens_auto_crop_scale(width, height, params) as f32
    } else {
        1.0
    };

    // Reference computes vr/vb in f32 arithmetic.
    let tca_vr_f32 = params.tca_vr as f32;
    let tca_vb_f32 = params.tca_vb as f32;
    let tca_amt_f32 = params.lens_tca_amount as f32;
    let vr = if (tca_vr_f32 - 1.0).abs() > 1e-5 {
        tca_vr_f32 + (1.0 - tca_vr_f32) * (1.0 - tca_amt_f32)
    } else {
        1.0
    };
    let vb = if (tca_vb_f32 - 1.0).abs() > 1e-5 {
        tca_vb_f32 + (1.0 - tca_vb_f32) * (1.0 - tca_amt_f32)
    } else {
        1.0
    };
    let has_tca = params.lens_tca_enabled && ((vr - 1.0).abs() > 1e-5 || (vb - 1.0).abs() > 1e-5);

    let vk1 = params.vig_k1;
    let vk2 = params.vig_k2;
    let vk3 = params.vig_k3;
    let lens_vig_amt = params.lens_vignette_amount * 0.8;
    let has_vignetting = params.lens_vignette_enabled
        && (vk1.abs() > 1e-6 || vk2.abs() > 1e-6 || vk3.abs() > 1e-6)
        && lens_vig_amt > 0.01;

    let mut out = LinearImage::new(width, height);
    let out_rows = out.rgb_mut();
    let out_stride = width_usize * 3;

    let tca_ctx = TcaContext {
        src: image,
        src_width: width_usize,
        src_height: height_usize,
        cx,
        cy,
    };

    out_rows
        .par_chunks_exact_mut(out_stride)
        .enumerate()
        .for_each(|(y, row)| {
            let y_f = y as f32;
            for (x, chunk) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                // Identity inverse transform: src = dst.
                let mut src_x = x as f32;
                let mut src_y = y_f;

                if auto_crop_scale > 1.0 {
                    src_x = cx + (src_x - cx) / auto_crop_scale;
                    src_y = cy + (src_y - cy) / auto_crop_scale;
                }

                if has_lens_correction {
                    let dx = (src_x - cx) as f64;
                    let dy = (src_y - cy) as f64;
                    let ru = (dx * dx + dy * dy).sqrt();

                    if ru > 1e-6 {
                        let ru_norm = ru / hd;
                        let ru_norm2 = ru_norm * ru_norm;

                        let rd_norm = if is_ptlens {
                            let a = lk1;
                            let b = lk2;
                            let c = lk3;
                            let d = 1.0 - a - b - c;
                            ru_norm * (a * ru_norm2 * ru_norm + b * ru_norm2 + c * ru_norm + d)
                        } else {
                            ru_norm
                                * (1.0
                                    + lk1 * ru_norm2
                                    + lk2 * (ru_norm2 * ru_norm2)
                                    + lk3 * (ru_norm2 * ru_norm2 * ru_norm2))
                        };

                        let effective_r_norm = ru_norm + (rd_norm - ru_norm) * lens_dist_amt;
                        let scale = effective_r_norm / ru_norm;

                        src_x = cx + (dx * scale) as f32;
                        src_y = cy + (dy * scale) as f32;
                    }
                }

                if k_distortion.abs() > 1e-5 {
                    let dx = (src_x - cx) as f64;
                    let dy = (src_y - cy) as f64;
                    let r2_norm = (dx * dx + dy * dy) * max_radius_sq_inv;
                    let f = 1.0 + k_distortion * r2_norm;

                    src_x = cx + (dx * f) as f32;
                    src_y = cy + (dy * f) as f32;
                }

                let mut pixel = [0.0f32; 3];

                if has_tca {
                    interpolate_pixel_with_tca(&tca_ctx, src_x, src_y, vr, vb, &mut pixel);
                } else {
                    interpolate_pixel(image, width_usize, height_usize, src_x, src_y, &mut pixel);
                }

                if has_vignetting {
                    let dx = (src_x - cx) as f64;
                    let dy = (src_y - cy) as f64;
                    let ru = (dx * dx + dy * dy).sqrt();
                    let ru_norm = ru / hd;
                    let ru_norm2 = ru_norm * ru_norm;

                    let v_factor = 1.0
                        + vk1 * ru_norm2
                        + vk2 * (ru_norm2 * ru_norm2)
                        + vk3 * (ru_norm2 * ru_norm2 * ru_norm2);

                    if v_factor > 1e-6 {
                        let correction_gain = 1.0 / v_factor;
                        let final_gain = 1.0 + (correction_gain - 1.0) * lens_vig_amt;

                        pixel[0] *= final_gain as f32;
                        pixel[1] *= final_gain as f32;
                        pixel[2] *= final_gain as f32;
                    }
                }

                *chunk = pixel;
            }
        });

    out
}
