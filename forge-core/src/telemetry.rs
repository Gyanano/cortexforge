//! Runtime telemetry engine — feedback loop core (§10).
//!
//! Three components:
//! - `TelemetryParser` — parses raw MCU output lines into `TelemetryRecord`s
//! - `AnomalyDetector` — sliding-window rule matching, 7 rule types
//! - `TelemetryCollector` — dual-source: file-based + debugger-based data collection

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;

use chrono::{DateTime, FixedOffset, Utc};

use crate::config::FeedbackChannelConfig;
use crate::error::ForgeResult;
use crate::protocol::{
    AnomalySeverity, ExpectationRule, ExpectationRuleType, TelemetryExpectation, TelemetryFormat,
    TelemetryRecord, TelemetryStream,
};

// ─── TelemetryParser ──────────────────────────────────────────────────────

/// Parses raw telemetry data lines into structured `TelemetryRecord` values.
pub struct TelemetryParser;

impl TelemetryParser {
    /// Parse a single raw telemetry line according to the declared stream format.
    pub fn parse_line(
        line: &str,
        stream: &TelemetryStream,
        source: &str,
    ) -> Option<TelemetryRecord> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        let ts: DateTime<FixedOffset> = Utc::now().into();
        let mut parsed = BTreeMap::new();

        match stream.format {
            TelemetryFormat::KeyValue => {
                for part in line.split(';') {
                    let part = part.trim();
                    if let Some((k, v)) = part.split_once(':') {
                        parsed.insert(k.trim().to_string(), v.trim().to_string());
                    }
                }
            }
            TelemetryFormat::Csv => {
                let fields: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
                for (i, val) in fields.iter().enumerate() {
                    parsed.insert(format!("field_{i}"), val.to_string());
                }
            }
            TelemetryFormat::Json => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                    if let Some(obj) = json.as_object() {
                        for (k, v) in obj {
                            parsed.insert(k.clone(), v.to_string());
                        }
                    }
                } else {
                    return None;
                }
            }
            TelemetryFormat::Hex => {
                // Store as raw only; no structured parsing for hex dumps
                parsed.insert("hex".into(), line.to_string());
            }
        }

        Some(TelemetryRecord {
            ts,
            stream: stream.name.clone(),
            source: source.to_string(),
            channel: stream.channel.clone(),
            raw: line.to_string(),
            parsed,
        })
    }

    /// Parse a line using a dynamic format (when no stream declaration is available).
    pub fn parse_line_dynamic(
        line: &str,
        stream_name: &str,
        channel: &str,
        source: &str,
    ) -> TelemetryRecord {
        let ts: DateTime<FixedOffset> = Utc::now().into();
        let line = line.trim();
        let mut parsed = BTreeMap::new();

        // Try key-value first, then CSV
        if line.contains(':') {
            for part in line.split(';') {
                if let Some((k, v)) = part.trim().split_once(':') {
                    parsed.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        } else if line.contains(',') {
            for (i, val) in line.split(',').enumerate() {
                parsed.insert(format!("field_{i}"), val.trim().to_string());
            }
        } else {
            parsed.insert("value".into(), line.to_string());
        }

        TelemetryRecord {
            ts,
            stream: stream_name.to_string(),
            source: source.to_string(),
            channel: channel.to_string(),
            raw: line.to_string(),
            parsed,
        }
    }
}

// ─── AnomalyDetector ──────────────────────────────────────────────────────

/// A single anomaly finding from rule evaluation.
#[derive(Debug, Clone)]
pub struct AnomalyFinding {
    pub stream: String,
    pub severity: AnomalySeverity,
    pub expected: String,
    pub actual: String,
    pub record_ts: DateTime<FixedOffset>,
}

/// Rule-based anomaly detector with sliding window per stream.
pub struct AnomalyDetector {
    rules: HashMap<String, Vec<ExpectationRule>>, // stream_name -> rules
    window: HashMap<String, VecDeque<TelemetryRecord>>, // stream_name -> recent records
    window_size: usize,
    last_seen: HashMap<String, DateTime<FixedOffset>>, // for heartbeat detection
}

impl AnomalyDetector {
    /// Create a new detector from telemetry expectations.
    pub fn new(expectations: &TelemetryExpectation, window_size: usize) -> Self {
        let mut rules: HashMap<String, Vec<ExpectationRule>> = HashMap::new();
        for rule in &expectations.expect {
            rules.entry(rule.stream.clone()).or_default().push(rule.clone());
        }
        Self { rules, window: HashMap::new(), window_size, last_seen: HashMap::new() }
    }

    /// Feed a new telemetry record and return any detected anomalies.
    pub fn feed(&mut self, record: &TelemetryRecord) -> Vec<AnomalyFinding> {
        let mut findings = Vec::new();

        // Update sliding window
        let window = self
            .window
            .entry(record.stream.clone())
            .or_insert_with(|| VecDeque::with_capacity(self.window_size));
        window.push_back(record.clone());
        if window.len() > self.window_size {
            window.pop_front();
        }

        // Update last seen timestamp
        self.last_seen.insert(record.stream.clone(), record.ts);

        // Evaluate all rules for this stream
        let rules = match self.rules.get(&record.stream) {
            Some(r) => r.clone(),
            None => return findings,
        };

        for rule in &rules {
            if let Some(finding) = self.evaluate_rule(rule, record) {
                findings.push(finding);
            }
        }

        findings
    }

    /// Evaluate a single expectation rule against the current record + window.
    fn evaluate_rule(
        &self,
        rule: &ExpectationRule,
        record: &TelemetryRecord,
    ) -> Option<AnomalyFinding> {
        match &rule.rule_type {
            ExpectationRuleType::Range { min, max } => {
                // Get the first numeric value from parsed
                for val in record.parsed.values() {
                    if let Ok(num) = val.parse::<f64>() {
                        if num < *min || num > *max {
                            return Some(AnomalyFinding {
                                stream: record.stream.clone(),
                                severity: rule.severity,
                                expected: format!("[{min}, {max}]"),
                                actual: val.clone(),
                                record_ts: record.ts,
                            });
                        }
                    }
                }
                None
            }
            ExpectationRuleType::Equals { value } => {
                for val in record.parsed.values() {
                    if val != value {
                        return Some(AnomalyFinding {
                            stream: record.stream.clone(),
                            severity: rule.severity,
                            expected: value.clone(),
                            actual: val.clone(),
                            record_ts: record.ts,
                        });
                    }
                }
                // If no parsed values, check raw
                if record.parsed.is_empty() && record.raw != *value {
                    return Some(AnomalyFinding {
                        stream: record.stream.clone(),
                        severity: rule.severity,
                        expected: value.clone(),
                        actual: record.raw.clone(),
                        record_ts: record.ts,
                    });
                }
                None
            }
            ExpectationRuleType::Contains { substring } => {
                if !record.raw.contains(substring.as_str()) {
                    return Some(AnomalyFinding {
                        stream: record.stream.clone(),
                        severity: rule.severity,
                        expected: format!("contains \"{substring}\""),
                        actual: record.raw.clone(),
                        record_ts: record.ts,
                    });
                }
                None
            }
            ExpectationRuleType::Matches { pattern } => {
                // Simple substring/regex match
                let matched = record.raw.contains(pattern.as_str());
                if !matched {
                    return Some(AnomalyFinding {
                        stream: record.stream.clone(),
                        severity: rule.severity,
                        expected: format!("matches \"{pattern}\""),
                        actual: record.raw.clone(),
                        record_ts: record.ts,
                    });
                }
                None
            }
            ExpectationRuleType::MonotonicIncreasing => {
                // Get the window and check values
                let window = self.window.get(&record.stream)?;
                if window.len() < 2 {
                    return None;
                }
                let prev = &window[window.len() - 2];
                let cur = record;
                // Compare all numeric values
                for (k, cur_val) in &cur.parsed {
                    if let (Ok(cur_num), Some(prev_val)) =
                        (cur_val.parse::<f64>(), prev.parsed.get(k))
                    {
                        if let Ok(prev_num) = prev_val.parse::<f64>() {
                            if cur_num < prev_num {
                                return Some(AnomalyFinding {
                                    stream: record.stream.clone(),
                                    severity: rule.severity,
                                    expected: format!("monotonic increasing ({k})"),
                                    actual: format!("{cur_num} < {prev_num}"),
                                    record_ts: record.ts,
                                });
                            }
                        }
                    }
                }
                None
            }
            ExpectationRuleType::Heartbeat { max_gap_sec } => {
                // Check time gap from the previous record in the window
                let window = self.window.get(&record.stream)?;
                if window.len() >= 2 {
                    let prev = &window[window.len() - 2];
                    let gap = record.ts.signed_duration_since(prev.ts);
                    if gap.num_seconds().abs() > *max_gap_sec as i64 {
                        return Some(AnomalyFinding {
                            stream: record.stream.clone(),
                            severity: rule.severity,
                            expected: format!("heartbeat within {max_gap_sec}s"),
                            actual: format!("gap of {}s", gap.num_seconds()),
                            record_ts: record.ts,
                        });
                    }
                }
                None
            }
            ExpectationRuleType::NoError { error_substrings } => {
                let raw_lower = record.raw.to_lowercase();
                for err in error_substrings {
                    if raw_lower.contains(&err.to_lowercase()) {
                        return Some(AnomalyFinding {
                            stream: record.stream.clone(),
                            severity: rule.severity,
                            expected: format!("no error containing \"{err}\""),
                            actual: record.raw.clone(),
                            record_ts: record.ts,
                        });
                    }
                }
                None
            }
        }
    }

    /// Load a detector from a node's expectations file.
    pub fn load_expectations(path: &Path, window_size: usize) -> ForgeResult<Self> {
        let expectations = crate::safe_read_toml::<TelemetryExpectation>(path).unwrap_or_default();
        Ok(Self::new(&expectations, window_size))
    }
}

// ─── TelemetryCollector ──────────────────────────────────────────────────

/// Collects telemetry data from both file-based and debugger-based sources.
pub struct TelemetryCollector;

impl TelemetryCollector {
    /// Scan a node's telemetry directory for new TOML records since a given timestamp.
    /// Returns records newer than `since`, and moves processed files to a `processed/` subdirectory.
    pub fn scan_file_records(
        telemetry_dir: &Path,
        since: DateTime<FixedOffset>,
    ) -> ForgeResult<Vec<TelemetryRecord>> {
        let mut records = Vec::new();

        if !telemetry_dir.exists() {
            return Ok(records);
        }

        let entries = std::fs::read_dir(telemetry_dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            // Skip directories and non-TOML files
            if path.is_dir() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }

            // Skip declaration and expectations files
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "telemetry_declaration.toml" || name == "expectations.toml" {
                continue;
            }

            // Load and filter by timestamp
            if let Ok(record) = TelemetryRecord::load(&path) {
                if record.ts > since {
                    records.push(record);
                }
            }

            // Move processed file
            let processed_dir = telemetry_dir.join("processed");
            std::fs::create_dir_all(&processed_dir)?;
            let dest = processed_dir.join(entry.file_name());
            let _ = std::fs::rename(&path, &dest);
        }

        // Sort by timestamp
        records.sort_by_key(|r| r.ts);
        Ok(records)
    }

    /// Check if any debugger-based channels are configured.
    #[must_use]
    pub fn has_debugger_channels(channels: &[FeedbackChannelConfig]) -> bool {
        channels.iter().any(|c| {
            matches!(
                c.channel_type,
                crate::config::ChannelType::Swo | crate::config::ChannelType::Rtt
            )
        })
    }

    /// Read MCU memory/registers via a debugger tool (pyocd, OpenOCD, JLink).
    ///
    /// MVP: spawns a subprocess to query the debugger. Returns raw bytes.
    /// The specific tool is auto-detected from the channel configuration.
    #[allow(dead_code)] // debugger integration is MVP+
    pub fn debugger_read(
        _channel: &FeedbackChannelConfig,
        _address: u32,
        _length: u32,
    ) -> ForgeResult<Vec<u8>> {
        // Future: spawn `pyocd commander -c "read8 <address>"` etc.
        // For MVP, this is a stub — file-based telemetry is the primary path.
        Err(crate::error::ForgeError::telemetry(
            "debugger-based telemetry not yet implemented (MVP uses file protocol)",
        ))
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TelemetryStream;

    fn test_stream() -> TelemetryStream {
        TelemetryStream {
            name: "temp_sensor".into(),
            channel: "debug-uart".into(),
            format: TelemetryFormat::KeyValue,
            rate_hz: Some(10.0),
            desc: "Temperature sensor readings".into(),
        }
    }

    #[test]
    fn test_parser_keyvalue() {
        let stream = test_stream();
        let record =
            TelemetryParser::parse_line("temp: 25.5; hum: 60", &stream, "test-node").unwrap();
        assert_eq!(record.stream, "temp_sensor");
        assert_eq!(record.parsed.get("temp").unwrap(), "25.5");
        assert_eq!(record.parsed.get("hum").unwrap(), "60");
    }

    #[test]
    fn test_parser_csv() {
        let mut stream = test_stream();
        stream.format = TelemetryFormat::Csv;
        let record = TelemetryParser::parse_line("25.5,60,1023", &stream, "test-node").unwrap();
        assert_eq!(record.parsed.get("field_0").unwrap(), "25.5");
        assert_eq!(record.parsed.get("field_1").unwrap(), "60");
    }

    #[test]
    fn test_parser_empty_line() {
        let stream = test_stream();
        assert!(TelemetryParser::parse_line("", &stream, "test-node").is_none());
        assert!(TelemetryParser::parse_line("   ", &stream, "test-node").is_none());
    }

    #[test]
    fn test_anomaly_detector_range() {
        let expectations = TelemetryExpectation {
            expect: vec![ExpectationRule {
                stream: "temp_sensor".into(),
                rule_type: ExpectationRuleType::Range { min: 0.0, max: 85.0 },
                params: BTreeMap::new(),
                severity: AnomalySeverity::Critical,
            }],
        };
        let mut detector = AnomalyDetector::new(&expectations, 10);

        // Normal value — no anomaly
        let stream = test_stream();
        let record = TelemetryParser::parse_line("temp: 25.5", &stream, "n1").unwrap();
        assert!(detector.feed(&record).is_empty());

        // Anomalous value
        let record2 = TelemetryParser::parse_line("temp: 99.0", &stream, "n1").unwrap();
        let findings = detector.feed(&record2);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, AnomalySeverity::Critical);
    }

    #[test]
    fn test_anomaly_detector_no_error() {
        let expectations = TelemetryExpectation {
            expect: vec![ExpectationRule {
                stream: "temp_sensor".into(),
                rule_type: ExpectationRuleType::NoError {
                    error_substrings: vec!["panic".into(), "hardfault".into(), "overflow".into()],
                },
                params: BTreeMap::new(),
                severity: AnomalySeverity::Critical,
            }],
        };
        let mut detector = AnomalyDetector::new(&expectations, 10);

        let stream = test_stream();
        let good = TelemetryParser::parse_line("temp: 25.5", &stream, "n1").unwrap();
        assert!(detector.feed(&good).is_empty());

        let bad = TelemetryParser::parse_line("PANIC: hardfault at 0x0800", &stream, "n1").unwrap();
        let findings = detector.feed(&bad);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn test_anomaly_detector_contains() {
        let expectations = TelemetryExpectation {
            expect: vec![ExpectationRule {
                stream: "temp_sensor".into(),
                rule_type: ExpectationRuleType::Contains { substring: "OK".into() },
                params: BTreeMap::new(),
                severity: AnomalySeverity::Warning,
            }],
        };
        let mut detector = AnomalyDetector::new(&expectations, 10);

        let stream = test_stream();
        let good = TelemetryParser::parse_line("temp:25.5;status:OK", &stream, "n1").unwrap();
        assert!(detector.feed(&good).is_empty());

        let bad = TelemetryParser::parse_line("temp:25.5;status:ERR", &stream, "n1").unwrap();
        let findings = detector.feed(&bad);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn test_anomaly_detector_monotonic() {
        let expectations = TelemetryExpectation {
            expect: vec![ExpectationRule {
                stream: "temp_sensor".into(),
                rule_type: ExpectationRuleType::MonotonicIncreasing,
                params: BTreeMap::new(),
                severity: AnomalySeverity::Warning,
            }],
        };
        let mut detector = AnomalyDetector::new(&expectations, 10);
        let stream = test_stream();

        let r1 = TelemetryParser::parse_line("ticks: 100", &stream, "n1").unwrap();
        assert!(detector.feed(&r1).is_empty());

        let r2 = TelemetryParser::parse_line("ticks: 200", &stream, "n1").unwrap();
        assert!(detector.feed(&r2).is_empty());

        let r3 = TelemetryParser::parse_line("ticks: 150", &stream, "n1").unwrap();
        let findings = detector.feed(&r3);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn test_anomaly_detector_heartbeat() {
        let expectations = TelemetryExpectation {
            expect: vec![ExpectationRule {
                stream: "temp_sensor".into(),
                rule_type: ExpectationRuleType::Heartbeat { max_gap_sec: 1.0 },
                params: BTreeMap::new(),
                severity: AnomalySeverity::Critical,
            }],
        };
        let mut detector = AnomalyDetector::new(&expectations, 10);
        let stream = test_stream();

        // First record — no gap yet
        let r1 = TelemetryParser::parse_line("alive: 1", &stream, "n1").unwrap();
        assert!(detector.feed(&r1).is_empty());

        // Second record — gap is < 1s (same function call), no anomaly
        let r2 = TelemetryParser::parse_line("alive: 1", &stream, "n1").unwrap();
        assert!(detector.feed(&r2).is_empty());
    }

    #[test]
    fn test_telemetry_collector_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let telemetry_dir = dir.path().join(".forge/telemetry");
        std::fs::create_dir_all(&telemetry_dir).unwrap();

        let since: DateTime<FixedOffset> = Utc::now().into();
        let records = TelemetryCollector::scan_file_records(&telemetry_dir, since).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn test_parse_line_dynamic() {
        let record = TelemetryParser::parse_line_dynamic("voltage: 3300", "power", "adc", "n1");
        assert_eq!(record.stream, "power");
        assert_eq!(record.parsed.get("voltage").unwrap(), "3300");
    }
}
