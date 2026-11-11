//! The Grafana dashboard and Prometheus alerts in `examples/grafana` only
//! use metrics PgDog exports, and show every result cache metric.

use std::collections::BTreeSet;

use regex::Regex;

use super::http_server::render;

const DASHBOARD: &str = include_str!("../../../examples/grafana/dashboard.json");
const ALERTS: &str = include_str!("../../../examples/grafana/prometheus/alerts.yml");
const PROMETHEUS_RULE: &str =
    include_str!("../../../examples/grafana/kubernetes/prometheusrule.yaml");

/// PromQL words that aren't metric names.
const KEYWORDS: &[&str] = &[
    "by",
    "without",
    "on",
    "ignoring",
    "group_left",
    "group_right",
    "and",
    "or",
    "unless",
    "bool",
    "offset",
];

/// Metrics Prometheus adds itself.
const PROMETHEUS_METRICS: &[&str] = &["up"];

/// Names of the metrics the OpenMetrics endpoint serves.
fn exported() -> BTreeSet<String> {
    crate::config::load_test();
    render()
        .lines()
        .filter_map(|line| line.strip_prefix("# TYPE "))
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// Metric names an expression reads.
fn metrics_in(expr: &str) -> BTreeSet<String> {
    let strip = [
        r#""[^"]*""#,                                                     // strings
        r"\{[^}]*\}",                                                     // label matchers
        r"\[[^\]]*\]",                                                    // ranges
        r"\$\{?\w+\}?",                                                   // Grafana variables
        r"\b(by|without|on|ignoring|group_left|group_right)\s*\([^)]*\)", // label lists
    ];
    let mut expr = expr.to_owned();
    for pattern in strip {
        expr = Regex::new(pattern)
            .unwrap()
            .replace_all(&expr, " ")
            .into_owned();
    }
    Regex::new(r"[a-zA-Z_:][a-zA-Z0-9_:]*(\s*\()?")
        .unwrap()
        .captures_iter(&expr)
        .filter(|capture| capture.get(1).is_none()) // functions
        .map(|capture| capture[0].trim().to_owned())
        .filter(|name| !KEYWORDS.contains(&name.as_str()))
        .collect()
}

/// PromQL expressions of the dashboard's panels and variables.
fn dashboard_exprs() -> Vec<String> {
    fn walk(value: &serde_json::Value, exprs: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(object) => {
                for (key, value) in object {
                    match (key.as_str(), value) {
                        ("expr" | "definition", serde_json::Value::String(expr)) => {
                            exprs.push(expr.clone())
                        }
                        _ => walk(value, exprs),
                    }
                }
            }
            serde_json::Value::Array(array) => array.iter().for_each(|value| walk(value, exprs)),
            _ => (),
        }
    }
    let dashboard: serde_json::Value = serde_json::from_str(DASHBOARD).unwrap();
    let mut exprs = vec![];
    walk(&dashboard, &mut exprs);
    // label_values(metric, label): the metric is what matters.
    exprs
        .into_iter()
        .map(|expr| {
            Regex::new(r"label_values\((\w+),\s*\w+\)")
                .unwrap()
                .replace_all(&expr, "$1")
                .into_owned()
        })
        .collect()
}

fn rule_exprs(rules: &str) -> Vec<String> {
    rules
        .lines()
        .filter_map(|line| line.trim().strip_prefix("expr: "))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn test_dashboard_and_alerts_use_exported_metrics() {
    let exported = exported();
    let exprs = dashboard_exprs()
        .into_iter()
        .chain(rule_exprs(ALERTS))
        .collect::<Vec<_>>();
    assert!(exprs.len() > 20, "found only {} expressions", exprs.len());

    for expr in exprs {
        for metric in metrics_in(&expr) {
            assert!(
                exported.contains(&metric) || PROMETHEUS_METRICS.contains(&metric.as_str()),
                "{metric} isn't exported by PgDog (in `{expr}`)"
            );
        }
    }
}

#[tokio::test]
async fn test_dashboard_shows_every_result_cache_metric() {
    let used: BTreeSet<_> = dashboard_exprs()
        .iter()
        .flat_map(|expr| metrics_in(expr))
        .collect();
    for metric in exported() {
        if metric.starts_with("result_cache_") {
            assert!(
                used.contains(&metric),
                "{metric} missing from the dashboard"
            );
        }
    }
}

#[test]
fn test_prometheus_rule_matches_alerts() {
    assert_eq!(rule_exprs(PROMETHEUS_RULE), rule_exprs(ALERTS));
}

#[test]
fn test_metrics_in() {
    let expr = r#"sum by (database) (rate(total_query_time{instance=~"$instance"}[$__rate_interval])) / on(instance) clients"#;
    assert_eq!(
        metrics_in(expr),
        BTreeSet::from(["total_query_time".to_owned(), "clients".to_owned()])
    );
}
