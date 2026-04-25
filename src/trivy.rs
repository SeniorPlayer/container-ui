//! Permissive parser for `trivy image --format json` output.
//!
//! The trivy schema is large and version-dependent; we only pull the fields
//! we care about (target, vulnerabilities) and tolerate everything else.

use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
pub struct Raw {
    #[serde(rename = "Results", default)]
    pub results: Vec<RawTarget>,
    #[serde(rename = "ArtifactName", default)]
    pub artifact: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct RawTarget {
    #[serde(rename = "Target", default)]
    pub target: String,
    #[serde(rename = "Vulnerabilities", default)]
    pub vulnerabilities: Vec<RawVuln>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct RawVuln {
    #[serde(rename = "VulnerabilityID", default)]
    pub id: String,
    #[serde(rename = "PkgName", default)]
    pub pkg: String,
    #[serde(rename = "InstalledVersion", default)]
    pub installed: String,
    #[serde(rename = "FixedVersion", default)]
    pub fixed: String,
    #[serde(rename = "Severity", default)]
    pub severity: String,
    #[serde(rename = "Title", default)]
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub artifact: String,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // `target` is parsed but not yet rendered
pub struct Finding {
    pub target: String,
    pub id: String,
    pub pkg: String,
    pub installed: String,
    pub fixed: String,
    pub severity: Severity,
    pub title: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Unknown,
}

impl Severity {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "CRITICAL" => Self::Critical,
            "HIGH" => Self::High,
            "MEDIUM" => Self::Medium,
            "LOW" => Self::Low,
            _ => Self::Unknown,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "CRITICAL",
            Self::High => "HIGH",
            Self::Medium => "MEDIUM",
            Self::Low => "LOW",
            Self::Unknown => "UNKNOWN",
        }
    }
}

impl Report {
    pub fn parse(json: &str) -> Option<Self> {
        let raw: Raw = serde_json::from_str(json).ok()?;
        let mut findings: Vec<Finding> = Vec::new();
        for tgt in raw.results {
            for v in tgt.vulnerabilities {
                findings.push(Finding {
                    target: tgt.target.clone(),
                    id: v.id,
                    pkg: v.pkg,
                    installed: v.installed,
                    fixed: v.fixed,
                    severity: Severity::parse(&v.severity),
                    title: v.title,
                });
            }
        }
        // Critical first.
        findings.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .then(a.id.cmp(&b.id))
        });
        Some(Self {
            artifact: raw.artifact.unwrap_or_default(),
            findings,
        })
    }

    pub fn counts(&self) -> [(Severity, usize); 5] {
        let mut c = std::collections::HashMap::new();
        for f in &self.findings {
            *c.entry(f.severity).or_insert(0) += 1;
        }
        [
            (Severity::Critical, *c.get(&Severity::Critical).unwrap_or(&0)),
            (Severity::High, *c.get(&Severity::High).unwrap_or(&0)),
            (Severity::Medium, *c.get(&Severity::Medium).unwrap_or(&0)),
            (Severity::Low, *c.get(&Severity::Low).unwrap_or(&0)),
            (Severity::Unknown, *c.get(&Severity::Unknown).unwrap_or(&0)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report() {
        let r = Report::parse(r#"{"Results":[]}"#).unwrap();
        assert!(r.findings.is_empty());
        assert_eq!(r.counts()[0], (Severity::Critical, 0));
    }

    #[test]
    fn one_finding() {
        let json = r#"{
            "ArtifactName": "alpine:3",
            "Results": [{
                "Target": "alpine:3 (alpine 3.18)",
                "Vulnerabilities": [{
                    "VulnerabilityID": "CVE-2024-0001",
                    "PkgName": "openssl",
                    "InstalledVersion": "3.1.0",
                    "FixedVersion": "3.1.1",
                    "Severity": "HIGH",
                    "Title": "buffer overflow in openssl"
                }]
            }]
        }"#;
        let r = Report::parse(json).unwrap();
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].severity, Severity::High);
        assert_eq!(r.findings[0].pkg, "openssl");
    }

    #[test]
    fn sort_order() {
        let json = r#"{
            "Results": [{
                "Target": "x",
                "Vulnerabilities": [
                    {"VulnerabilityID": "CVE-Z", "Severity": "LOW"},
                    {"VulnerabilityID": "CVE-A", "Severity": "CRITICAL"},
                    {"VulnerabilityID": "CVE-M", "Severity": "MEDIUM"}
                ]
            }]
        }"#;
        let r = Report::parse(json).unwrap();
        assert_eq!(r.findings[0].severity, Severity::Critical);
        assert_eq!(r.findings[2].severity, Severity::Low);
    }
}
