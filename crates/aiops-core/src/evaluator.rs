//! The only code that decides whether a candidate is accepted.
//!
//! Gates run in a fixed order and the first gate that fires decides the outcome:
//!
//! 1. patch refused by policy → `POLICY_REJECTED`; build or runner failure → `EXECUTION_ERROR`
//! 2. any golden case differs → `REJECTED`
//! 3. noisy or incomplete measurement → `INCONCLUSIVE`
//! 4. errors, timeouts or crashes got worse → `REJECTED`
//! 5. median p95 improvement ≥ target and every pair improved → `VERIFIED`
//! 6. pairs disagree on direction → `INCONCLUSIVE`
//! 7. consistent but short of the target (or a regression) → `REJECTED`

use serde::{Deserialize, Serialize};

use crate::report::{BaselineReport, Pair, TrialReport};
use crate::scenario::Contract;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Outcome {
    Verified,
    Rejected,
    Inconclusive,
    /// Build failure, runner failure or cancellation. Not a verdict on the idea.
    ExecutionError,
    /// The patch touched files it may not touch, or did not apply. Nothing was run.
    PolicyRejected,
}

impl Outcome {
    /// Verdicts are judgements about the candidate; the other outcomes are not.
    pub fn is_verdict(self) -> bool {
        matches!(
            self,
            Outcome::Verified | Outcome::Rejected | Outcome::Inconclusive
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Evaluation {
    pub outcome: Outcome,
    /// Which gate fired, in plain words. The first entry is the deciding reason.
    pub reasons: Vec<String>,
    /// Per-pair `(baseline p95 − candidate p95) / baseline p95`.
    pub improvements: Vec<f64>,
    pub median_improvement: Option<f64>,
    /// Run-level A/A noise used for gate 3, if a baseline job was supplied.
    pub aa_noise: Option<f64>,
    /// Spread of the baseline p95 across this trial's pairs.
    pub baseline_spread: Option<f64>,
}

/// Median relative p95 difference between the two sides of A/A pairs.
pub fn aa_noise(baseline: &BaselineReport) -> Option<f64> {
    let diffs: Vec<f64> = baseline
        .aa_pairs
        .iter()
        .filter(|p| p.baseline.p95_ms > 0.0)
        .map(|p| (p.baseline.p95_ms - p.candidate.p95_ms).abs() / p.baseline.p95_ms)
        .collect();
    median(&diffs)
}

pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mid = sorted.len() / 2;
    Some(if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    })
}

fn baseline_spread(pairs: &[Pair]) -> Option<f64> {
    let p95s: Vec<f64> = pairs.iter().map(|p| p.baseline.p95_ms).collect();
    let med = median(&p95s)?;
    if med <= 0.0 {
        return None;
    }
    let max = p95s.iter().copied().fold(f64::MIN, f64::max);
    let min = p95s.iter().copied().fold(f64::MAX, f64::min);
    Some((max - min) / med)
}

pub fn evaluate(contract: &Contract, report: &TrialReport, aa_noise: Option<f64>) -> Evaluation {
    let mut eval = Evaluation {
        outcome: Outcome::ExecutionError,
        reasons: Vec::new(),
        improvements: Vec::new(),
        median_improvement: None,
        aa_noise,
        baseline_spread: baseline_spread(&report.pairs),
    };
    // Always report the raw per-pair change, even when an earlier gate decides the outcome.
    if report.pairs.iter().all(|p| p.baseline.p95_ms > 0.0) {
        eval.improvements = report
            .pairs
            .iter()
            .map(|p| (p.baseline.p95_ms - p.candidate.p95_ms) / p.baseline.p95_ms)
            .collect();
        eval.median_improvement = median(&eval.improvements);
    }
    let decide = |mut eval: Evaluation, outcome: Outcome, reason: String| {
        eval.outcome = outcome;
        eval.reasons.insert(0, reason);
        eval
    };

    // Gate 1: nothing to judge.
    if let Some(err) = &report.patch_error {
        return decide(
            eval,
            Outcome::PolicyRejected,
            format!("patch refused: {err}"),
        );
    }
    if let Some(err) = &report.execution_error {
        return decide(
            eval,
            Outcome::ExecutionError,
            format!("runner error: {err}"),
        );
    }
    match &report.build {
        Some(build) if build.ok => {}
        Some(_) => {
            return decide(
                eval,
                Outcome::ExecutionError,
                "candidate build failed".into(),
            )
        }
        None => {
            return decide(
                eval,
                Outcome::ExecutionError,
                "candidate was not built".into(),
            )
        }
    }
    if let Some(base) = &report.baseline_correctness {
        if !base.all_passed() {
            return decide(
                eval,
                Outcome::ExecutionError,
                format!(
                    "baseline no longer matches golden cases: {:?}",
                    base.failed_names()
                ),
            );
        }
    }

    // Gate 2: correctness comes before speed.
    match &report.correctness {
        Some(c) if c.all_passed() => {}
        Some(c) => {
            return decide(
                eval,
                Outcome::Rejected,
                format!(
                    "golden cases failed ({}/{} passed): {:?}",
                    c.passed,
                    c.total,
                    c.failed_names()
                ),
            )
        }
        None => {
            return decide(
                eval,
                Outcome::ExecutionError,
                "correctness was not checked".into(),
            )
        }
    }
    if let Some(crash) = &report.candidate_crashed {
        return decide(
            eval,
            Outcome::Rejected,
            format!("candidate process died: {crash}"),
        );
    }

    // Gate 3: is the measurement trustworthy?
    if (report.pairs.len() as u32) < contract.pairs {
        return decide(
            eval,
            Outcome::Inconclusive,
            format!(
                "only {} of {} measurement pairs completed",
                report.pairs.len(),
                contract.pairs
            ),
        );
    }
    if let Some(noise) = aa_noise {
        if noise > contract.max_noise {
            return decide(
                eval,
                Outcome::Inconclusive,
                format!(
                    "A/A noise {:.1}% exceeds {:.1}%",
                    noise * 100.0,
                    contract.max_noise * 100.0
                ),
            );
        }
    }
    if let Some(spread) = eval.baseline_spread {
        if spread > contract.max_noise {
            return decide(
                eval,
                Outcome::Inconclusive,
                format!(
                    "baseline p95 varied {:.1}% across pairs (limit {:.1}%)",
                    spread * 100.0,
                    contract.max_noise * 100.0
                ),
            );
        }
    }
    for pair in &report.pairs {
        for run in [&pair.baseline, &pair.candidate] {
            if run.dropped > 0 {
                return decide(
                    eval,
                    Outcome::Inconclusive,
                    format!(
                        "pair {}: load generator dropped {} {} iterations",
                        pair.index, run.dropped, run.label
                    ),
                );
            }
            if !run.drained() {
                return decide(
                    eval,
                    Outcome::Inconclusive,
                    format!(
                        "pair {}: {} finished {} of {} offered iterations",
                        pair.index, run.label, run.iterations, run.offered
                    ),
                );
            }
        }
        if pair.baseline.error_rate() > contract.max_baseline_error_rate {
            return decide(
                eval,
                Outcome::Inconclusive,
                format!(
                    "pair {}: baseline error rate {:.2}% means the environment is unhealthy",
                    pair.index,
                    pair.baseline.error_rate() * 100.0
                ),
            );
        }
        if pair.baseline.p95_ms <= 0.0 {
            return decide(
                eval,
                Outcome::Inconclusive,
                format!("pair {}: baseline has no successful responses", pair.index),
            );
        }
    }

    // Gate 4: no regression in errors or completed work.
    for pair in &report.pairs {
        let (b, c) = (&pair.baseline, &pair.candidate);
        if c.error_rate() > b.error_rate() + contract.max_error_rate_increase {
            return decide(
                eval,
                Outcome::Rejected,
                format!(
                    "pair {}: candidate error rate {:.2}% vs baseline {:.2}%",
                    pair.index,
                    c.error_rate() * 100.0,
                    b.error_rate() * 100.0
                ),
            );
        }
        if c.success_ratio() + contract.max_error_rate_increase < b.success_ratio() {
            return decide(
                eval,
                Outcome::Rejected,
                format!(
                    "pair {}: candidate completed {:.2}% of offered requests vs baseline {:.2}%",
                    pair.index,
                    c.success_ratio() * 100.0,
                    b.success_ratio() * 100.0
                ),
            );
        }
    }

    // Gates 5–7: the performance claim itself.
    let med = eval
        .median_improvement
        .expect("at least one pair with a positive baseline p95");
    let all_better = eval.improvements.iter().all(|&i| i > 0.0);
    let all_worse = eval.improvements.iter().all(|&i| i <= 0.0);
    let pct = |x: f64| format!("{:.1}%", x * 100.0);

    if all_better && med >= contract.min_improvement {
        let reason = format!(
            "median p95 improvement {} ≥ {} and all {} pairs improved",
            pct(med),
            pct(contract.min_improvement),
            eval.improvements.len()
        );
        return decide(eval, Outcome::Verified, reason);
    }
    if !all_better && !all_worse {
        let reason = format!(
            "pairs disagree on direction: {:?}",
            eval.improvements
                .iter()
                .map(|&i| pct(i))
                .collect::<Vec<_>>()
        );
        return decide(eval, Outcome::Inconclusive, reason);
    }
    let reason = if all_worse {
        format!("candidate is slower: median p95 change {}", pct(med))
    } else {
        format!(
            "median p95 improvement {} is below the {} target",
            pct(med),
            pct(contract.min_improvement)
        )
    };
    decide(eval, Outcome::Rejected, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{BuildReport, CaseResult, CorrectnessReport, LoadStats};

    fn contract() -> Contract {
        Contract {
            pairs: 3,
            min_improvement: 0.20,
            max_noise: 0.15,
            max_error_rate_increase: 0.0,
            max_baseline_error_rate: 0.01,
        }
    }

    fn run(label: &str, p95: f64) -> LoadStats {
        LoadStats {
            label: label.into(),
            rate: 100,
            duration_s: 20,
            planned: 2000,
            offered: 2000,
            iterations: 2000,
            dropped: 0,
            ok: 2000,
            bad_status: 0,
            timeouts: 0,
            net_errors: 0,
            p50_ms: p95 / 2.0,
            p95_ms: p95,
            p99_ms: p95 * 1.2,
            max_ms: p95 * 2.0,
        }
    }

    fn pair(i: u32, b: f64, c: f64) -> Pair {
        Pair {
            index: i,
            order: if i.is_multiple_of(2) { "BC" } else { "CB" }.into(),
            baseline: run("baseline", b),
            candidate: run("candidate", c),
        }
    }

    fn correctness(passed: bool) -> CorrectnessReport {
        CorrectnessReport {
            total: 2,
            passed: if passed { 2 } else { 1 },
            cases: vec![
                CaseResult {
                    name: "a".into(),
                    passed: true,
                    detail: None,
                },
                CaseResult {
                    name: "b".into(),
                    passed,
                    detail: (!passed).then(|| "differs".into()),
                },
            ],
        }
    }

    fn report(pairs: Vec<Pair>) -> TrialReport {
        TrialReport {
            build: Some(BuildReport {
                ok: true,
                cached: false,
                duration_ms: 1,
                log_sha256: String::new(),
                log_tail: String::new(),
            }),
            correctness: Some(correctness(true)),
            baseline_correctness: Some(correctness(true)),
            pairs,
            ..Default::default()
        }
    }

    #[test]
    fn verified_when_every_pair_clears_the_target() {
        let e = evaluate(
            &contract(),
            &report(vec![
                pair(0, 10.0, 2.0),
                pair(1, 10.5, 2.1),
                pair(2, 10.2, 1.9),
            ]),
            Some(0.03),
        );
        assert_eq!(e.outcome, Outcome::Verified, "{:?}", e.reasons);
        assert!(e.median_improvement.unwrap() > 0.79);
    }

    #[test]
    fn wrong_answers_are_rejected_before_speed_is_considered() {
        let mut r = report(vec![
            pair(0, 10.0, 1.0),
            pair(1, 10.0, 1.0),
            pair(2, 10.0, 1.0),
        ]);
        r.correctness = Some(correctness(false));
        let e = evaluate(&contract(), &r, None);
        assert_eq!(e.outcome, Outcome::Rejected);
        assert!(e.reasons[0].contains("golden"));
    }

    #[test]
    fn build_failure_is_not_a_verdict() {
        let mut r = report(vec![]);
        r.build.as_mut().unwrap().ok = false;
        let e = evaluate(&contract(), &r, None);
        assert_eq!(e.outcome, Outcome::ExecutionError);
        assert!(!e.outcome.is_verdict());
    }

    #[test]
    fn policy_rejection_wins_over_everything() {
        let mut r = report(vec![]);
        r.patch_error = Some("`Cargo.toml` is explicitly protected".into());
        assert_eq!(
            evaluate(&contract(), &r, None).outcome,
            Outcome::PolicyRejected
        );
    }

    #[test]
    fn noisy_environment_is_inconclusive() {
        let r = report(vec![
            pair(0, 10.0, 2.0),
            pair(1, 10.0, 2.0),
            pair(2, 10.0, 2.0),
        ]);
        assert_eq!(
            evaluate(&contract(), &r, Some(0.30)).outcome,
            Outcome::Inconclusive
        );

        let r = report(vec![
            pair(0, 10.0, 2.0),
            pair(1, 14.0, 2.0),
            pair(2, 10.0, 2.0),
        ]);
        let e = evaluate(&contract(), &r, None);
        assert_eq!(e.outcome, Outcome::Inconclusive);
        assert!(e.reasons[0].contains("varied"));
        assert_eq!(
            e.improvements.len(),
            3,
            "the measured change is reported even when inconclusive"
        );
    }

    #[test]
    fn dropped_iterations_make_the_result_inconclusive() {
        let mut r = report(vec![
            pair(0, 10.0, 2.0),
            pair(1, 10.0, 2.0),
            pair(2, 10.0, 2.0),
        ]);
        r.pairs[1].candidate.dropped = 5;
        r.pairs[1].candidate.iterations -= 5;
        assert_eq!(
            evaluate(&contract(), &r, None).outcome,
            Outcome::Inconclusive
        );
    }

    #[test]
    fn undrained_runs_make_the_result_inconclusive() {
        let mut r = report(vec![
            pair(0, 10.0, 2.0),
            pair(1, 10.0, 2.0),
            pair(2, 10.0, 2.0),
        ]);
        r.pairs[2].candidate.iterations = 1990;
        assert_eq!(
            evaluate(&contract(), &r, None).outcome,
            Outcome::Inconclusive
        );
    }

    #[test]
    fn faster_but_failing_requests_is_rejected() {
        let mut r = report(vec![
            pair(0, 10.0, 2.0),
            pair(1, 10.0, 2.0),
            pair(2, 10.0, 2.0),
        ]);
        r.pairs[0].candidate.ok -= 3;
        r.pairs[0].candidate.timeouts = 3;
        let e = evaluate(&contract(), &r, None);
        assert_eq!(e.outcome, Outcome::Rejected);
        assert!(e.reasons[0].contains("error rate"));
    }

    #[test]
    fn crash_is_rejected() {
        let mut r = report(vec![
            pair(0, 10.0, 2.0),
            pair(1, 10.0, 2.0),
            pair(2, 10.0, 2.0),
        ]);
        r.candidate_crashed = Some("killed by signal 9".into());
        assert_eq!(evaluate(&contract(), &r, None).outcome, Outcome::Rejected);
    }

    #[test]
    fn mixed_directions_are_inconclusive() {
        let r = report(vec![
            pair(0, 10.0, 7.0),
            pair(1, 10.0, 11.0),
            pair(2, 10.0, 7.0),
        ]);
        assert_eq!(
            evaluate(&contract(), &r, None).outcome,
            Outcome::Inconclusive
        );
    }

    #[test]
    fn consistent_but_small_gain_is_rejected() {
        let r = report(vec![
            pair(0, 10.0, 9.0),
            pair(1, 10.0, 9.2),
            pair(2, 10.0, 8.9),
        ]);
        let e = evaluate(&contract(), &r, None);
        assert_eq!(e.outcome, Outcome::Rejected);
        assert!(e.reasons[0].contains("below"));
    }

    #[test]
    fn regression_is_rejected() {
        let r = report(vec![
            pair(0, 10.0, 12.0),
            pair(1, 10.0, 12.0),
            pair(2, 10.0, 12.5),
        ]);
        let e = evaluate(&contract(), &r, None);
        assert_eq!(e.outcome, Outcome::Rejected);
        assert!(e.reasons[0].contains("slower"));
    }

    #[test]
    fn too_few_pairs_is_inconclusive() {
        let r = report(vec![pair(0, 10.0, 2.0)]);
        assert_eq!(
            evaluate(&contract(), &r, None).outcome,
            Outcome::Inconclusive
        );
    }

    #[test]
    fn median_handles_even_and_odd() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(&[]), None);
    }
}
