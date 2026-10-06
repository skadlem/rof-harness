//! Thin external-gate runner. See research/bench-landscape.md checklist A–E.
//! Primary: Terminal-Bench 2.1 frozen slice via Harbor protocol. Second:
//! Multi-SWE-bench Rust slice + flash. SWE-bench Verified calibration-only.
//! Outcome taxonomy rule: infra failures are never scored as capability.

#[cfg(test)]
use std::path::{Path, PathBuf};

mod engine;
mod gate;
mod matrix;
mod oracle;
mod report;
mod slices;

pub use engine::{ContainerOutcome, DockerEngine, Engine};
pub use gate::{paired_bootstrap, wilson_ci};
pub use matrix::{CellReps, ComparisonReport, MatchedPair, ReportFlag};
pub use oracle::{
    apply_patch, check_swe_results, check_tb_reward, preflight, run_eval_sh, swe_test_status,
    Verdict,
};
pub use report::{instance_report, InstanceReport, RunReport};
pub use slices::{
    check_winnability, load_swe_slice, load_tb_slice, slice_path, Instance, SweInstance, ToolCaps,
    DEFAULT_TIMEOUT_SECS,
};
#[cfg(test)]
pub(crate) use slices::{task_artifacts, toml_int};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("eval-test-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn tb_task(root: &Path, name: &str, image: &str, timeout: u64) {
        let t = root.join(name);
        std::fs::create_dir_all(t.join("solution")).unwrap();
        std::fs::create_dir_all(t.join("tests")).unwrap();
        std::fs::write(
            t.join("task.toml"),
            format!("image = \"{image}\"\ntimeout = {timeout}\n"),
        )
        .unwrap();
        std::fs::write(t.join("instruction.md"), format!("# {name}\nDo it.\n")).unwrap();
        std::fs::write(t.join("solution").join("solve.sh"), "#!/bin/sh\necho ok\n").unwrap();
        std::fs::write(t.join("tests").join("test.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    }

    fn inst(id: &str, oracle: &str, tests: Vec<String>) -> Instance {
        Instance {
            id: id.into(),
            instruction: "x".into(),
            image: "img".into(),
            oracle: oracle.into(),
            tests,
            timeout_secs: 60,
        }
    }

    #[test]
    fn tb_adapter_walks_fixture_tasks_dir() {
        let root = tmp();
        tb_task(&root, "aaa", "img-a", 300);
        tb_task(&root, "bbb", "img-b", 600);
        let all = load_tb_slice(&root, &[]).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "aaa"); // sorted
        assert!(all[0].instruction.contains("aaa"));
        assert_eq!(all[0].image, "img-a");
        assert!(all[0].oracle.contains("echo ok"));
        assert_eq!(all[0].tests, vec!["test.sh".to_string()]);
        assert_eq!(all[0].timeout_secs, 300);
        let one = load_tb_slice(&root, &["bbb".to_string()]).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].image, "img-b");
    }

    #[test]
    fn toml_int_parses_floats_by_truncation() {
        let keys = &["timeout"];
        assert_eq!(toml_int("timeout = 900\n", keys), Some(900));
        assert_eq!(toml_int("timeout = 900.0\n", keys), Some(900));
        assert_eq!(toml_int("timeout = 0.5\n", keys), Some(0));
        assert_eq!(toml_int("timeout = fast\n", keys), None);
    }

    #[test]
    fn swe_adapter_reads_both_families() {
        let root = tmp();
        let f = root.join("instances.jsonl");
        std::fs::write(
            &f,
            "{\"instance_id\":\"django__django-11099\",\"repo\":\"django\",\"FAIL_TO_PASS\":[\"t1\",\"t2\"],\"PASS_TO_PASS\":[\"t3\"],\"test_patch\":\"p\"}\n\
             {\"org\":\"acme\",\"repo\":\"lib\",\"number\":7,\"FAIL_TO_PASS\":\"['a']\",\"PASS_TO_PASS\":[],\"fix_patch\":\"q\"}\n",
        )
        .unwrap();
        let all = load_swe_slice(&f, &[]).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "django__django-11099");
        assert_eq!(
            all[0].fail_to_pass,
            vec!["t1".to_string(), "t2".to_string()]
        );
        assert_eq!(all[0].pass_to_pass, vec!["t3".to_string()]);
        assert_eq!(all[0].test_patch, "p");
        assert_eq!(all[1].id, "acme__lib-7"); // Multi-SWE org/repo/number keying
        assert_eq!(all[1].fail_to_pass, vec!["a".to_string()]);
        let one = load_swe_slice(&f, &["acme__lib-7".to_string()]).unwrap();
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn swe_verdict_needs_all_f2p_and_no_p2p_regress() {
        let f2p = vec!["t1".to_string(), "t2".to_string()];
        let p2p = vec!["t3".to_string()];
        assert_eq!(
            check_swe_results(&f2p, &p2p, "PASS t1\nPASS t2\nPASS t3\n"),
            Verdict::Resolved
        );
        assert_eq!(
            check_swe_results(&f2p, &p2p, "PASS t1\nFAIL t2\nPASS t3\n"),
            Verdict::Unresolved
        );
        assert_eq!(
            check_swe_results(&f2p, &p2p, "PASS t1\nPASS t2\nFAIL t3\n"),
            Verdict::Unresolved
        );
    }

    #[test]
    fn taxonomy_infra_is_never_capability() {
        assert_eq!(
            check_swe_results(&["t".to_string()], &[], "INFRA: container failed"),
            Verdict::InfraFailure
        );
        assert_eq!(
            check_swe_results(&["t".to_string()], &[], ""),
            Verdict::ErrorNoReport
        );
        assert_eq!(
            check_swe_results(&["t".to_string()], &[], "EMPTY_PATCH"),
            Verdict::EmptyPatch
        );
        assert_eq!(
            check_swe_results(&["t".to_string()], &[], "AMBIGUOUS output"),
            Verdict::Ambiguous
        );
        assert_eq!(check_swe_results(&[], &[], "PASS t\n"), Verdict::Ambiguous);
        for v in [
            Verdict::InfraFailure,
            Verdict::ErrorNoReport,
            Verdict::EmptyPatch,
            Verdict::Ambiguous,
        ] {
            assert_ne!(v, Verdict::Resolved); // infra/errors never score as capability
            assert_ne!(v, Verdict::Unresolved);
        }
    }

    struct FakeEngine(ContainerOutcome);
    impl Engine for FakeEngine {
        fn run_instance(&self, _i: &Instance, _w: &Path) -> std::io::Result<ContainerOutcome> {
            Ok(ContainerOutcome {
                reward: self.0.reward,
                logs: self.0.logs.clone(),
            })
        }
    }

    #[test]
    fn engine_trait_fake_needs_no_docker() {
        let e = FakeEngine(ContainerOutcome {
            reward: Some(true),
            logs: "ok".into(),
        });
        let out = e
            .run_instance(&inst("i", "o", vec!["t".into()]), Path::new("/tmp"))
            .unwrap();
        assert_eq!(out.reward, Some(true));
    }

    #[test]
    fn preflight_excludes_broken_before_spend() {
        let good = inst("good", "solve", vec!["test.sh".into()]);
        let bad_oracle = inst("bad-oracle", "", vec!["test.sh".into()]);
        let bad_tests = inst("bad-tests", "solve", vec![]);
        let excluded = preflight(&[good, bad_oracle, bad_tests]);
        assert_eq!(
            excluded,
            vec!["bad-oracle".to_string(), "bad-tests".to_string()]
        );
    }

    #[test]
    fn reward_file_txt_and_json() {
        let root = tmp();
        let txt = root.join("reward.txt");
        std::fs::write(&txt, "1\n").unwrap();
        assert_eq!(check_tb_reward(&txt).unwrap(), Verdict::Resolved);
        std::fs::write(&txt, "0\n").unwrap();
        assert_eq!(check_tb_reward(&txt).unwrap(), Verdict::Unresolved);
        let js = root.join("reward.json");
        std::fs::write(&js, "{\"reward\": 1}").unwrap();
        assert_eq!(check_tb_reward(&js).unwrap(), Verdict::Resolved);
        std::fs::write(&js, "{\"reward\": \"INFRA\"}").unwrap();
        assert_eq!(check_tb_reward(&js).unwrap(), Verdict::InfraFailure);
    }

    #[test]
    fn wilson_math_on_known_values() {
        assert_eq!(wilson_ci(0, 0), (0.0, 0.0, 0.0));
        let (p, lo, hi) = wilson_ci(60, 100);
        assert!((p - 0.6).abs() < 1e-12);
        assert!((lo - 0.502).abs() < 0.005 && (hi - 0.691).abs() < 0.005);
        assert!(lo < p && p < hi); // never a bare percentage
        let (_, lo89, hi89) = wilson_ci(45, 89);
        assert!(hi89 - lo89 > 0.15); // 89 tasks => wide intervals
    }

    #[test]
    fn instance_report_reads_cumulative_usage_steps_and_halt() {
        let dir = std::env::temp_dir().join(format!("rof-ir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("events.jsonl");
        std::fs::write(
            &p,
            concat!(
                "{\"type\":\"MessageEnd\",\"id\":1,\"message\":{\"role\":\"Assistant\",\"content\":\"\"},\"interrupted\":false,\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"cache_read_tokens\":3,\"reasoning_tokens\":1,\"cost_usd\":null}}\n",
                "{\"type\":\"TurnEnd\",\"turn\":1,\"reason\":\"BudgetExceeded\",\"usage_totals\":{\"input_tokens\":10,\"output_tokens\":2,\"cache_read_tokens\":3,\"reasoning_tokens\":1,\"cost_usd\":null}}\n",
                "{\"type\":\"MessageEnd\",\"id\":2,\"message\":{\"role\":\"Assistant\",\"content\":\"\"},\"interrupted\":false,\"usage\":null}\n",
                "{\"type\":\"TurnEnd\",\"turn\":1,\"reason\":\"BudgetExceeded\",\"usage_totals\":{\"input_tokens\":25,\"output_tokens\":6,\"cache_read_tokens\":9,\"reasoning_tokens\":4,\"cost_usd\":0.5}}\n",
                "{\"type\":\"RunEnd\",\"outcome\":{\"Failed\":\"steps\"},\"messages\":[]}\n",
            ),
        )
        .unwrap();
        let r = instance_report("t1", Verdict::Resolved, &p, 12, "diff --git a").unwrap();
        assert_eq!(
            (r.tokens_in, r.tokens_out),
            (Some(25), Some(6)),
            "last cumulative TurnEnd"
        );
        assert_eq!(r.dollars, Some(0.5), "cost_usd flows into dollars");
        assert_eq!(r.steps, 2);
        assert_eq!(r.halt_reason.as_deref(), Some("steps"));
        assert_eq!(r.wall_secs, 12);
        let same = instance_report("t1", Verdict::Resolved, &p, 12, "diff --git a").unwrap();
        assert_eq!(r.patch_digest, same.patch_digest, "digest is stable");
        let other = instance_report("t1", Verdict::Resolved, &p, 12, "diff --git b").unwrap();
        assert_ne!(r.patch_digest, other.patch_digest);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpriced_usage_reports_null_not_zero() {
        let dir = tmp();
        let p = dir.join("events.jsonl");
        std::fs::write(
            &p,
            concat!(
                "{\"type\":\"MessageEnd\",\"id\":1,\"message\":{\"role\":\"Assistant\",\"content\":\"\"},\"interrupted\":false,\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"cache_read_tokens\":3,\"reasoning_tokens\":1,\"cost_usd\":null}}\n",
                "{\"type\":\"TurnEnd\",\"turn\":1,\"reason\":\"BudgetExceeded\",\"usage_totals\":{\"input_tokens\":10,\"output_tokens\":2,\"cache_read_tokens\":3,\"reasoning_tokens\":1,\"cost_usd\":null}}\n",
                "{\"type\":\"RunEnd\",\"outcome\":{\"Failed\":\"tokens\"},\"messages\":[]}\n",
            ),
        )
        .unwrap();
        let r = instance_report("t1", Verdict::Unresolved, &p, 3, "").unwrap();
        assert_eq!(r.dollars, None, "unpriced usage is absence, not $0.00");
        let v = serde_json::to_value(&r).unwrap();
        assert!(v["dollars"].is_null(), "absence must serialise to null");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn instance_report_without_turn_end_reports_null_not_zero() {
        let dir = tmp();
        let p = dir.join("events.jsonl");
        // torn dump: a MessageEnd is visible but no TurnEnd ever landed, so
        // the token counts are unknown — they must read null, not 0.
        std::fs::write(
            &p,
            "{\"type\":\"MessageEnd\",\"id\":1,\"message\":{\"role\":\"Assistant\",\"content\":\"\"},\"interrupted\":false,\"usage\":null}\n",
        )
        .unwrap();
        let r = instance_report("t1", Verdict::Unresolved, &p, 3, "").unwrap();
        assert_eq!(r.tokens_in, None, "no TurnEnd => unknown tokens, not 0");
        assert_eq!(r.tokens_out, None);
        assert_eq!(r.dollars, None);
        let v = serde_json::to_value(&r).unwrap();
        assert!(
            v["tokens_in"].is_null() && v["tokens_out"].is_null(),
            "absence must serialise to null: {v}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn paired_bootstrap_hand_checkable_and_deterministic() {
        assert!(paired_bootstrap(&[], 100, 1).is_none());
        let one = paired_bootstrap(&[(1.0, 0.0)], 100, 7).unwrap();
        assert_eq!(
            one,
            (1.0, 1.0, 1.0),
            "single pair is degenerate but defined"
        );
        let pairs = [(1.0, 0.0), (0.0, 1.0), (1.0, 0.0), (1.0, 0.0)];
        let (m, lo, hi) = paired_bootstrap(&pairs, 10_000, 42).unwrap();
        assert!((m - 0.5).abs() < 1e-12, "{m}");
        assert!(lo <= m && m <= hi, "[{lo}, {hi}] brackets the mean");
        assert_eq!(
            paired_bootstrap(&pairs, 10_000, 42).unwrap(),
            (m, lo, hi),
            "same seed, same gate"
        );
        let (_, lo2, hi2) = paired_bootstrap(&pairs, 10_000, 43).unwrap();
        assert!(hi2 > lo2, "interval has width on mixed pairs");
    }

    fn rep_cell(task: &str, passed: &[u32], total: u32) -> CellReps {
        CellReps {
            task: task.into(),
            reps: passed
                .iter()
                .map(|p| f64::from(*p) / f64::from(total))
                .collect(),
        }
    }

    #[test]
    fn flaky_fires_on_stored_swd_reps() {
        // Stored source: ~/.local/share/rof-tb/work/matrix-v1/matrix.json,
        // `rof_runs` rows with task == "session-window-debug" (the swd cell,
        // 3 single-shot reps): r1 6/7, r2 4/7, r3 5/7 (tests_passed /
        // tests_failed from the proxy verdicts). research/DECISIONS.md:102
        // cites the same cell as 5/7, 6/7, 5/7; either form disagrees across
        // reps and must flag flaky. Rival pi reps in the same file: 2/7 x3.
        let stored = MatchedPair {
            control: rep_cell("session-window-debug", &[6, 4, 5], 7),
            treatment: rep_cell("session-window-debug", &[2, 2, 2], 7),
        };
        assert_eq!(
            ComparisonReport::from_pairs(&[stored], 0).flags,
            vec![ReportFlag::Flaky]
        );
        let cited = rep_cell("session-window-debug", &[5, 6, 5], 7);
        let cited = MatchedPair {
            control: cited.clone(),
            treatment: cited,
        };
        assert_eq!(
            ComparisonReport::from_pairs(&[cited], 0).flags,
            vec![ReportFlag::Flaky]
        );
    }

    #[test]
    fn blocked_or_empty_pairs_withhold_the_rate() {
        let pair = || MatchedPair {
            control: rep_cell("t", &[2, 2, 2], 4),
            treatment: rep_cell("t", &[3, 3, 3], 4),
        };
        let empty = ComparisonReport::from_pairs(&[], 0);
        assert_eq!(empty.control_pass_rate, None);
        assert_eq!(empty.treatment_pass_rate, None);
        assert_eq!(empty.lift, None);
        let blocked = ComparisonReport::from_pairs(&[pair()], 1);
        assert_eq!(blocked.control_pass_rate, None);
        assert_eq!(blocked.treatment_pass_rate, None);
        assert_eq!(blocked.lift, None);
        assert_eq!((blocked.eligible_pairs, blocked.blocked_pairs), (1, 1));
        // An unmeasured cell (no reps) withholds too — it is never read as 0.
        let unmeasured = ComparisonReport::from_pairs(
            &[MatchedPair {
                control: rep_cell("t", &[], 4),
                treatment: rep_cell("t", &[3, 3, 3], 4),
            }],
            0,
        );
        assert_eq!(unmeasured.control_pass_rate, None);
        assert_eq!(unmeasured.eligible_pairs, 0);
        let published = ComparisonReport::from_pairs(&[pair()], 0);
        assert_eq!(published.control_pass_rate, Some(0.5));
        assert_eq!(published.treatment_pass_rate, Some(0.75));
        assert_eq!(published.lift, Some(0.25));
    }

    #[test]
    fn unanimous_cell_with_headroom_is_not_flaky() {
        let pair = MatchedPair {
            control: rep_cell("t", &[2, 2, 2], 4),
            treatment: rep_cell("t", &[3, 3, 3], 4),
        };
        let report = ComparisonReport::from_pairs(&[pair], 0);
        assert!(report.flags.is_empty(), "{:?}", report.flags);
        assert_eq!(report.control_pass_rate, Some(0.5));
        assert_eq!(report.treatment_pass_rate, Some(0.75));
    }

    #[test]
    fn saturation_flags_on_all_pass_or_all_fail_arms() {
        let pair = |control: &[u32], treatment: &[u32]| MatchedPair {
            control: rep_cell("t", control, 4),
            treatment: rep_cell("t", treatment, 4),
        };
        let pass = ComparisonReport::from_pairs(&[pair(&[4, 4, 4], &[2, 2, 2])], 0);
        assert!(pass.flags.contains(&ReportFlag::ControlSaturatedPass));
        assert!(!pass.flags.contains(&ReportFlag::Flaky));
        let fail = ComparisonReport::from_pairs(&[pair(&[0, 0, 0], &[2, 2, 2])], 0);
        assert!(fail.flags.contains(&ReportFlag::ControlSaturatedFail));
        let t_pass = ComparisonReport::from_pairs(&[pair(&[2, 2, 2], &[4, 4, 4])], 0);
        assert!(t_pass.flags.contains(&ReportFlag::TreatmentSaturatedPass));
        let t_fail = ComparisonReport::from_pairs(&[pair(&[2, 2, 2], &[0, 0, 0])], 0);
        assert!(t_fail.flags.contains(&ReportFlag::TreatmentSaturatedFail));
        // Withheld rates make no headroom claim: no saturation flag fires.
        let withheld = ComparisonReport::from_pairs(&[pair(&[4, 4, 4], &[2, 2, 2])], 1);
        assert_eq!(withheld.flags, Vec::<ReportFlag>::new());
    }

    #[test]
    fn tokens_per_solved_guards_division() {
        let rep = |id: &str, v: Verdict, t: Option<u64>| InstanceReport {
            id: id.into(),
            verdict: v,
            tokens_in: t,
            tokens_out: Some(0),
            dollars: None,
            wall_secs: 0,
            steps: 0,
            halt_reason: None,
            patch_digest: String::new(),
        };
        let empty = RunReport { instances: vec![] };
        assert_eq!(empty.tokens_per_solved(), None);
        let r = RunReport {
            instances: vec![
                rep("a", Verdict::Resolved, Some(100)),
                rep("b", Verdict::Unresolved, Some(100)),
            ],
        };
        assert_eq!(r.tokens_per_solved(), Some(200.0));
        let unknown = RunReport {
            instances: vec![
                rep("a", Verdict::Resolved, Some(100)),
                rep("b", Verdict::Unresolved, None),
            ],
        };
        assert_eq!(
            unknown.tokens_per_solved(),
            None,
            "an unknown token count makes the total unknown, not smaller"
        );
    }

    #[test]
    fn slice_ids_live_in_repo() {
        let p = slice_path("tb-slice-a");
        assert_eq!(p.file_name().unwrap(), "tb-slice-a.json");
        let raw = std::fs::read_to_string(&p).expect("frozen slice file must exist");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v.get("ids").and_then(|x| x.as_array()).is_some());
    }

    fn win_task(root: &Path, artifacts: &str, seed_files: &[&str]) -> PathBuf {
        let t = root.join("task");
        std::fs::create_dir_all(t.join("tests")).unwrap();
        std::fs::create_dir_all(t.join("solution")).unwrap();
        std::fs::write(t.join("tests").join("test.sh"), "exit 0\n").unwrap();
        std::fs::write(t.join("solution").join("solve.sh"), "echo ok\n").unwrap();
        std::fs::write(t.join("task.toml"), format!("artifacts = [{artifacts}]\n")).unwrap();
        for f in seed_files {
            let p = t.join("environment").join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "seed\n").unwrap();
        }
        t
    }

    fn full_caps() -> ToolCaps {
        ToolCaps {
            can_create: true,
            can_patch: true,
            exec_prefixes: vec!["python3".into()],
        }
    }

    #[test]
    fn winnability_pass_and_missing_write() {
        let root = tmp();
        // Present artifact needs patch; absent needs create.
        let t = win_task(
            &root,
            "\"/app/data\", \"/app/output/config.json\"",
            &["data/x"],
        );
        assert!(check_winnability(&t, &full_caps()).is_empty());
        let no_write = ToolCaps {
            can_create: false,
            ..full_caps()
        };
        let bad = check_winnability(&t, &no_write);
        assert_eq!(bad.len(), 1);
        assert!(bad[0].contains("no write tool"), "{bad:?}");
    }

    #[test]
    fn winnability_missing_tests_or_oracle() {
        let root = tmp();
        let t = win_task(&root, "\"/app/data\"", &["data/x"]);
        std::fs::remove_dir_all(t.join("tests")).unwrap();
        let bad = check_winnability(&t, &full_caps());
        assert!(
            bad.iter().any(|b| b.contains("no verifier tests")),
            "{bad:?}"
        );
        assert!(!bad.iter().any(|b| b.contains("no oracle")), "{bad:?}");

        // missing-oracle branch: tests restored, solve.sh gone.
        std::fs::create_dir_all(t.join("tests")).unwrap();
        std::fs::write(t.join("tests").join("test.sh"), "exit 0\n").unwrap();
        std::fs::remove_file(t.join("solution").join("solve.sh")).unwrap();
        let bad = check_winnability(&t, &full_caps());
        assert!(
            bad.iter().any(|b| b.contains("no oracle solve.sh")),
            "{bad:?}"
        );
        assert!(
            !bad.iter().any(|b| b.contains("no verifier tests")),
            "{bad:?}"
        );
    }

    #[test]
    fn artifacts_multiline_array_parses() {
        let root = tmp();
        let t = win_task(&root, "\"/app/data\"", &["data/x"]);
        std::fs::write(
            t.join("task.toml"),
            "image = \"img\"\nartifacts = [\n  \"/app/data\",\n  \"/app/output\",\n]\n",
        )
        .unwrap();
        assert!(check_winnability(&t, &full_caps()).is_empty());
        let no_write = ToolCaps {
            can_create: false,
            ..full_caps()
        };
        let bad = check_winnability(&t, &no_write);
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(bad[0].contains("no write tool"), "{bad:?}");
    }

    #[test]
    fn artifacts_comment_lines_tolerated() {
        let toml = "# task metadata\nimage = \"img\"\n\nartifacts = [\n  # what the verifier reads\n  \"/app/data\", # seed present\n]\ntimeout = 60\n";
        assert_eq!(task_artifacts(toml).unwrap(), vec!["/app/data".to_string()]);
        assert!(task_artifacts("# nothing here\n").unwrap().is_empty());
    }

    #[test]
    fn artifacts_malformed_is_loud_error() {
        // unquoted entry
        assert!(task_artifacts("artifacts = [\n  /app/data,\n]").is_err());
        // array never closed
        assert!(task_artifacts("artifacts = [\"/app/data\"\n").is_err());
        // through check_winnability: never an empty ("winnable") pass
        let root = tmp();
        let t = win_task(&root, "\"/app/data\"", &["data/x"]);
        std::fs::write(t.join("task.toml"), "artifacts = [/app/data]\n").unwrap();
        let bad = check_winnability(&t, &full_caps());
        assert!(bad.iter().any(|b| b.contains("unparseable")), "{bad:?}");
    }

    #[test]
    fn winnability_missing_task_toml_is_violation() {
        let root = tmp();
        let t = win_task(&root, "\"/app/data\"", &["data/x"]);
        std::fs::remove_file(t.join("task.toml")).unwrap();
        let bad = check_winnability(&t, &full_caps());
        assert!(bad.iter().any(|b| b.contains("task.toml")), "{bad:?}");
    }

    #[test]
    fn frozen_slices_pinned_by_digest_and_ids() {
        // Re-freezing any slice must break `cargo test` before any run.
        for (name, sha, ids) in [
            (
                "tb-slice-a",
                "a1fbef407c28d415fb29fe052fe91164fc07fd277f66f38fd17c37bd581f2813",
                21usize,
            ),
            (
                "tb-slice-v1seed",
                "a460a97aff950f2dc1041fd2207b6f5751c73bd207df7b361bf190ae3dde3525",
                2usize,
            ),
            (
                "multiswe-rust",
                "badd9685eb31cd5675465e29294c321e168a1678321613ed028b30fbadd07b36",
                50usize,
            ),
        ] {
            let p = slice_path(name); // CARGO_MANIFEST_DIR/slices/<name>.json
            let out = std::process::Command::new("sha256sum")
                .arg(&p)
                .output()
                .expect("sha256sum must run");
            let got = String::from_utf8(out.stdout).unwrap();
            assert!(got.starts_with(sha), "{name}: slice re-frozen? got {got}");
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            assert_eq!(v["ids"].as_array().unwrap().len(), ids, "{name} ids");
        }
    }

    #[ignore]
    #[test]
    fn winnability_frozen_tasks_live() {
        // Env-gated like live_smoke: ROF_TB_TASKS or default share path; skip if absent.
        let base = std::env::var("ROF_TB_TASKS").unwrap_or_else(|_| {
            format!(
                "{}/.local/share/rof-tb/tasks",
                std::env::var("HOME").unwrap_or_default()
            )
        });
        let base = PathBuf::from(base);
        if !base.is_dir() {
            return;
        }
        let mut checked = 0;
        for entry in std::fs::read_dir(&base).unwrap().filter_map(|e| e.ok()) {
            if !entry.file_type().map(|f| f.is_dir()).unwrap_or(false) {
                continue;
            }
            let bad = check_winnability(&entry.path(), &full_caps());
            assert!(bad.is_empty(), "{:?}: {bad:?}", entry.file_name());
            checked += 1;
        }
        assert!(checked > 0, "no task dirs under {}", base.display());
    }
}
