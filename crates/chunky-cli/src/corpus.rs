//! Corpus manifests (D-022 manifest v1, plus the multi-origin layout):
//! one `manifest.jsonl` per directory, bodies stored by SHA-256.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// One response from a manifest, with the fields the measurement tools use.
#[derive(Clone, Debug)]
pub struct Row {
    /// Directory holding the manifest.
    pub dir: PathBuf,
    /// Session or slice name (the directory's file name).
    pub group: String,
    pub id: String,
    pub site: String,
    pub route: String,
    pub url: String,
    pub body_sha256: String,
    pub decoded_bytes: usize,
    pub served_encoding: Option<String>,
    pub served_bytes: Option<usize>,
    /// Recovered logical flush positions (decoded offsets), ascending.
    pub flushes: Vec<usize>,
    /// RFC 8941 byte-sequence form of the dictionary hash, if served dcb.
    pub dictionary_sfv: Option<String>,
    pub flags: Vec<String>,
    /// Capture time (ISO 8601), for chronological order.
    pub captured: String,
    /// Excluded spans (decoded offsets, end exclusive): literal-only, never
    /// admitted to learned artifacts (D-017, D-022).
    pub excluded: Vec<(usize, usize)>,
    /// Held-out-origin split for multi-origin rows (train/tune/heldout).
    pub split: String,
}

/// Size bucket label for multi-origin reporting (D-026).
pub fn size_bucket(n: usize) -> &'static str {
    match n {
        0..1024 => "<1 KB",
        1024..10_240 => "1–10 KB",
        10_240..102_400 => "10–100 KB",
        102_400..1_048_576 => "100 KB–1 MB",
        _ => ">1 MB",
    }
}

/// Select an even spread of indices from an already ordered corpus, with
/// approximately `budget` decoded bytes. A positive budget selects at least
/// one nonempty body, even when a single response is larger than the budget.
pub fn spread_indices(sizes: &[usize], budget: usize) -> Vec<usize> {
    if budget == 0 {
        return Vec::new();
    }
    let total: usize = sizes.iter().sum();
    let step = (total as f64 / budget as f64).max(1.0);
    let mut out = Vec::new();
    let mut acc = 0.0;
    for (i, &size) in sizes.iter().enumerate() {
        if size == 0 {
            continue;
        }
        acc += 1.0;
        if acc >= step {
            acc -= step;
            out.push(i);
        }
    }
    if out.is_empty()
        && let Some(i) = sizes.iter().position(|&s| s > 0)
    {
        out.push(i);
    }
    out
}

impl Row {
    pub fn body_path(&self) -> PathBuf {
        let html = self
            .dir
            .join("bodies")
            .join(format!("{}.html", self.body_sha256));
        if html.exists() {
            html
        } else {
            self.dir.join("bodies").join(&self.body_sha256)
        }
    }

    pub fn body(&self) -> Result<Vec<u8>> {
        let p = self.body_path();
        let b = std::fs::read(&p).with_context(|| format!("read {}", p.display()))?;
        if b.len() != self.decoded_bytes {
            bail!(
                "{}: {} bytes, manifest says {}",
                p.display(),
                b.len(),
                self.decoded_bytes
            );
        }
        Ok(b)
    }

    /// The raw dictionary this response was served against, if any.
    pub fn dictionary_path(&self) -> Option<PathBuf> {
        let sfv = self.dictionary_sfv.as_ref()?;
        let hex = sfv_to_hex(sfv)?;
        Some(self.dir.join("dicts").join(format!("{hex}.dict")))
    }

    pub fn has_flag(&self, f: &str) -> bool {
        self.flags.iter().any(|x| x == f)
    }
}

fn sfv_to_hex(sfv: &str) -> Option<String> {
    let b64 = sfv.trim_matches(':');
    let bytes = base64_decode(b64)?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Load every row of `<dir>/manifest.jsonl`.
pub fn load_dir(dir: &Path) -> Result<Vec<Row>> {
    let mpath = dir.join("manifest.jsonl");
    let text =
        std::fs::read_to_string(&mpath).with_context(|| format!("read {}", mpath.display()))?;
    let group = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .with_context(|| format!("{}:{}: bad JSON", mpath.display(), i + 1))?;
        // Multi-origin rows: only bodies stored byte-exact (or unverifiable),
        // never empty ones.
        if let Some(f) = v.get("fidelity").and_then(Value::as_str)
            && f != "exact"
            && f != "unverified"
        {
            continue;
        }
        if v.get("body_sha256").is_none_or(Value::is_null) {
            continue;
        }
        // ... and none the extractors flagged as empty, missing or not
        // decoded to the original bytes.
        let bad_flag = v.get("flags").and_then(Value::as_array).is_some_and(|a| {
            a.iter().filter_map(Value::as_str).any(|f| {
                matches!(
                    f,
                    "empty" | "truncated" | "dechunk_failed" | "body_missing_locally"
                ) || f.starts_with("still_encoded")
            })
        });
        if bad_flag
            || v.get("decoded_bytes")
                .or_else(|| v.get("bytes"))
                .and_then(Value::as_u64)
                == Some(0)
        {
            continue;
        }
        let flushes = v
            .get("flushes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|f| f.get("decoded_offset").and_then(Value::as_u64))
                    .map(|x| x as usize)
                    .collect()
            })
            .unwrap_or_default();
        let flags = v
            .get("flags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|f| f.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        // Session manifests carry decoded_bytes/site/route; multi-origin ones
        // carry bytes/type and a size bucket stands in for the route.
        let decoded_bytes = v
            .get("decoded_bytes")
            .or_else(|| v.get("bytes"))
            .and_then(Value::as_u64)
            .context("row without decoded_bytes or bytes")? as usize;
        let site = match s(&v, "site") {
            x if x.is_empty() => s(&v, "type"),
            x => x,
        };
        let route = match s(&v, "route") {
            x if x.is_empty() => size_bucket(decoded_bytes).to_string(),
            x => x,
        };
        rows.push(Row {
            dir: dir.to_path_buf(),
            group: group.clone(),
            id: match s(&v, "id") {
                x if x.is_empty() => s(&v, "body_sha256"),
                x => x,
            },
            site,
            route,
            url: s(&v, "url"),
            body_sha256: s(&v, "body_sha256"),
            decoded_bytes,
            served_encoding: v
                .get("served_encoding")
                .and_then(Value::as_str)
                .map(String::from),
            served_bytes: v
                .get("served_bytes")
                .and_then(Value::as_u64)
                .map(|x| x as usize),
            flushes,
            dictionary_sfv: v
                .get("dictionary_sha256")
                .and_then(Value::as_str)
                .map(String::from),
            flags,
            captured: s(&v, "captured_utc"),
            excluded: v
                .get("excluded_spans")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            let x = x.as_array()?;
                            Some((x.first()?.as_u64()? as usize, x.get(1)?.as_u64()? as usize))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            split: s(&v, "split"),
        });
    }
    Ok(rows)
}

/// Load several manifest directories, dropping rows flagged as
/// interstitial/captcha/consent (excluded from every slice, D-022).
pub fn load(dirs: &[PathBuf]) -> Result<Vec<Row>> {
    let mut all = Vec::new();
    for d in dirs {
        all.extend(load_dir(d)?.into_iter().filter(|r| {
            !["interstitial", "captcha", "consent"]
                .iter()
                .any(|f| r.has_flag(f))
        }));
    }
    Ok(all)
}

/// Shared read-only dictionary bytes keyed by path, so each loads once.
#[derive(Default)]
pub struct Dicts(HashMap<PathBuf, Vec<u8>>);

impl Dicts {
    pub fn load(rows: &[Row]) -> Result<Self> {
        let mut m = HashMap::new();
        for r in rows {
            if let Some(p) = r.dictionary_path()
                && !m.contains_key(&p)
            {
                let b = std::fs::read(&p).with_context(|| format!("read {}", p.display()))?;
                m.insert(p, b);
            }
        }
        Ok(Self(m))
    }

    pub fn get(&self, r: &Row) -> Option<&[u8]> {
        r.dictionary_path()
            .and_then(|p| self.0.get(&p))
            .map(Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_spread_handles_small_budgets_and_empty_bodies() {
        assert_eq!(spread_indices(&[16 << 20], 8 << 20), vec![0]);
        assert_eq!(spread_indices(&[0, 1024], 1), vec![1]);
        assert_eq!(spread_indices(&[1024; 10], 2048), vec![4, 9]);
        assert!(spread_indices(&[1024], 0).is_empty());
        assert!(spread_indices(&[0, 0], 1024).is_empty());
        assert!(spread_indices(&[], 1024).is_empty());
    }

    #[test]
    fn sfv_hash_to_hex() {
        // sha256("") in RFC 8941 byte-sequence form.
        assert_eq!(
            sfv_to_hex(":47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=:").unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
