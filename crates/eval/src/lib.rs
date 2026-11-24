//! Thin external-gate runner.
//! Primary: Terminal-Bench 2.1 frozen slice via Harbor protocol.
//! Outcome taxonomy rule: infra failures are never scored as capability.

#[cfg(test)]
use std::path::{Path, PathBuf};

mod engine;
mod oracle;
mod report;
mod slices;

pub use engine::{ContainerOutcome, DockerEngine, Engine};
pub use oracle::{check_tb_reward, preflight, ApplyMode, Verdict};
pub use report::{
    instance_report_from_events, instance_report_with_mode, patch_digest, BudgetShapeLabel,
    Estimand, ExecPolicyLabel, InstanceReport, RunProvenance, RunReport, WorkdirSeedLabel,
};
pub use slices::{check_winnability, load_tb_slice, Instance, ToolCaps};
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
            agent_timeout_secs: 60,
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
    fn tb_adapter_reads_agent_and_verifier_timeouts_separately() {
        // Real task.toml files carry `timeout_sec` in both sections and they
        // differ per task (query-optimize 900 agent / 1800 verifier,
        // video-processing 3600/3600). The old `timeout_secs` key never matched
        // `timeout_sec`, so every cell silently got the 1800s default.
        let root = tmp();
        let task = root.join("z");
        std::fs::create_dir_all(task.join("tests")).unwrap();
        std::fs::write(
            task.join("task.toml"),
            "schema_version = \"1.1\"\n\n[verifier]\ntimeout_sec = 1800.0\n\n[agent]\ntimeout_sec = 900.0\n\n[environment]\ndocker_image = \"img\"\n",
        )
        .unwrap();
        std::fs::write(task.join("instruction.md"), "do it").unwrap();
        std::fs::write(task.join("tests").join("test.sh"), "true\n").unwrap();
        let all = load_tb_slice(&root, &[]).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].timeout_secs, 1800, "verifier budget");
        assert_eq!(all[0].agent_timeout_secs, 900, "agent budget");
    }

    #[test]
    fn toml_int_parses_floats_by_truncation() {
        let keys = &["timeout"];
        assert_eq!(toml_int("timeout = 900\n", keys), Some(900));
        assert_eq!(toml_int("timeout = 900.0\n", keys), Some(900));
        assert_eq!(toml_int("timeout = 0.5\n", keys), Some(0));
        assert_eq!(toml_int("timeout = fast\n", keys), None);
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
        let r = instance_report_with_mode(
            "t1",
            Verdict::Resolved,
            &p,
            12,
            "diff --git a",
            ApplyMode::Strict,
            None,
        )
        .unwrap();
        assert_eq!(
            (r.tokens_in, r.tokens_out),
            (Some(25), Some(6)),
            "last cumulative TurnEnd"
        );
        assert_eq!(r.dollars, Some(0.5), "cost_usd flows into dollars");
        assert_eq!(r.steps, 2);
        assert_eq!(r.halt_reason.as_deref(), Some("steps"));
        assert_eq!(r.wall_secs, 12);
        let same = instance_report_with_mode(
            "t1",
            Verdict::Resolved,
            &p,
            12,
            "diff --git a",
            ApplyMode::Strict,
            None,
        )
        .unwrap();
        assert_eq!(r.patch_digest, same.patch_digest, "digest is stable");
        let other = instance_report_with_mode(
            "t1",
            Verdict::Resolved,
            &p,
            12,
            "diff --git b",
            ApplyMode::Strict,
            None,
        )
        .unwrap();
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
        let r = instance_report_with_mode(
            "t1",
            Verdict::Unresolved,
            &p,
            3,
            "",
            ApplyMode::Strict,
            None,
        )
        .unwrap();
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
        let r = instance_report_with_mode(
            "t1",
            Verdict::Unresolved,
            &p,
            3,
            "",
            ApplyMode::Strict,
            None,
        )
        .unwrap();
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
    fn interrupted_provider_usage_is_unknown_in_report() {
        let dir = tmp();
        let events = dir.join("events.jsonl");
        std::fs::write(&events, concat!(
            "{\"type\":\"Error\",\"error\":{\"code\":\"wallclock-usage-unknown\",\"message\":\"interrupted\"}}\n",
            "{\"type\":\"TurnEnd\",\"turn\":1,\"reason\":\"BudgetExceeded\",\"usage_totals\":{\"input_tokens\":50,\"output_tokens\":10,\"cache_read_tokens\":0,\"reasoning_tokens\":null,\"cost_usd\":null}}\n"
        )).unwrap();
        for code in ["wallclock-usage-unknown", "provider-usage-unknown"] {
            let text = std::fs::read_to_string(&events)
                .unwrap()
                .replace("wallclock-usage-unknown", code);
            std::fs::write(&events, &text).unwrap();
            let report = instance_report_with_mode(
                "t",
                Verdict::Unresolved,
                &events,
                1,
                "",
                ApplyMode::Strict,
                None,
            )
            .unwrap();
            assert_eq!(
                (report.tokens_in, report.tokens_out, report.dollars),
                (None, None, None)
            );
        }
    }

    #[test]
    fn instance_report_from_events_reads_usage_steps_halt_and_wall_ms() {
        use agent_event::{
            AgentError, AgentEvent, Message, Role, RunOutcome, TurnEndReason, UsageReport,
        };
        let events = vec![
            AgentEvent::MessageEnd {
                id: 1,
                message: Message {
                    role: Role::Assistant,
                    content: String::new(),
                },
                interrupted: false,
                usage: None,
            },
            AgentEvent::TurnEnd {
                turn: 1,
                reason: TurnEndReason::Completed,
                usage_totals: UsageReport {
                    input_tokens: 40,
                    output_tokens: 12,
                    cache_read_tokens: 5,
                    reasoning_tokens: None,
                    cost_usd: Some(0.25),
                },
            },
            AgentEvent::RunEnd {
                outcome: RunOutcome::Aborted,
                messages: vec![],
            },
        ];
        let r = instance_report_from_events(
            "task-1".into(),
            Verdict::Resolved,
            &events,
            2_500,
            ApplyMode::Strict,
        );
        assert_eq!(r.id, "task-1");
        assert_eq!(r.verdict, Verdict::Resolved);
        assert_eq!(
            (r.tokens_in, r.tokens_out, r.dollars),
            (Some(40), Some(12), Some(0.25))
        );
        assert_eq!(r.wall_secs, 2);
        assert_eq!(r.steps, 1);
        assert_eq!(r.halt_reason.as_deref(), Some("aborted"));
        assert_eq!(r.apply_mode, ApplyMode::Strict);
        assert_eq!(r.apply_detail, None);
        assert!(r.patch_digest.is_empty());

        let mut unknown = events;
        unknown.push(AgentEvent::Error {
            error: AgentError {
                code: "provider-usage-unknown".into(),
                message: "missing".into(),
            },
        });
        let r_unknown = instance_report_from_events(
            "task-2".into(),
            Verdict::Unresolved,
            &unknown,
            999,
            ApplyMode::Lenient,
        );
        assert_eq!(
            (r_unknown.tokens_in, r_unknown.tokens_out, r_unknown.dollars),
            (None, None, None)
        );
        assert_eq!(r_unknown.wall_secs, 0);
        assert_eq!(r_unknown.apply_mode, ApplyMode::Lenient);
    }

    #[test]
    fn run_report_without_provenance_still_deserializes() {
        // A report written before the provenance block existed carries no
        // "provenance" key; serde(default) keeps it readable rather than
        // rejecting the file (and never fabricates a block).
        let r: RunReport = serde_json::from_str("{\"instances\": []}").unwrap();
        assert!(r.provenance.is_none());
        assert!(r.instances.is_empty());
    }

    #[test]
    fn a_report_written_before_workdir_seed_reads_as_unknown() {
        // Every banked report predates the field. It must read as "fidelity
        // unknown", never as a seed that would pool with a faithful row.
        let old = r#"{"instances":[],"provenance":{"binary_git_sha":"cd839a9",
            "estimand":"InContainerReward","budget_tokens":300000,"budget_steps":60,
            "budget_actions":120,"budget_shape":"Gross","exec_policy":"Any",
            "apply_mode":"Strict"}}"#;
        let r: RunReport = serde_json::from_str(old).expect("old report still parses");
        let p = r.provenance.expect("provenance present");
        assert_eq!(
            p.workdir_seed, None,
            "absent field is unknown, not faithful"
        );
    }

    #[test]
    fn run_report_provenance_round_trips() {
        let r = RunReport {
            instances: vec![],
            provenance: Some(RunProvenance {
                run_config: None,
                binary_git_sha: Some("b3a2597".into()),
                estimand: Estimand::InContainerReward,
                budget_tokens: 300_000,
                budget_steps: 60,
                budget_actions: 120,
                budget_shape: BudgetShapeLabel::MissPlusOut,
                exec_policy: ExecPolicyLabel::Any,
                workdir_seed: Some(WorkdirSeedLabel::ImageApp),
                reasoning_effort: Some("max".into()),
                steer: Some(true),
                apply_mode: ApplyMode::Strict,
            }),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["provenance"]["estimand"], "InContainerReward");
        assert_eq!(v["provenance"]["budget_tokens"], 300_000);
        assert_eq!(v["provenance"]["budget_steps"], 60);
        assert_eq!(v["provenance"]["budget_actions"], 120);
        assert_eq!(v["provenance"]["budget_shape"], "MissPlusOut");
        assert_eq!(v["provenance"]["workdir_seed"], "ImageApp");
        assert_eq!(v["provenance"]["exec_policy"], "Any");
        assert_eq!(v["provenance"]["apply_mode"], "Strict");
        let back: RunReport = serde_json::from_value(v).unwrap();
        assert_eq!(
            back.provenance.unwrap().estimand,
            Estimand::InContainerReward
        );
    }

    #[test]
    fn old_provenance_has_unknown_run_config() {
        let value = serde_json::json!({"instances":[], "provenance": {
            "binary_git_sha": "abc", "estimand": "InContainerReward",
            "budget_tokens": 1, "budget_steps": 1, "budget_actions": 1,
            "budget_shape": "Gross", "exec_policy": "Allowlist", "apply_mode": "Strict"
        }});
        let report: RunReport = serde_json::from_value(value).unwrap();
        assert!(serde_json::to_value(report).unwrap()["provenance"]
            .as_object()
            .unwrap()
            .contains_key("run_config"));
    }

    #[test]
    fn estimand_labels_are_stable() {
        // Report files record these exact strings; renaming a variant would
        // silently change every arm's label.
        for (label, want) in [
            (Estimand::Ungraded, "Ungraded"),
            (Estimand::HarborOracle, "HarborOracle"),
            (Estimand::InContainerReward, "InContainerReward"),
        ] {
            assert_eq!(serde_json::to_value(label).unwrap(), want);
        }
    }

    fn slice_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("slices")
            .join(format!("{name}.json"))
    }

    #[test]
    fn instance_report_with_mode_records_lenient_and_detail() {
        assert_eq!(ApplyMode::default(), ApplyMode::Strict);
        let d = tmp();
        let events = d.join("events.jsonl");
        std::fs::write(
            &events,
            "{\"type\":\"RunEnd\",\"outcome\":\"Passed\",\"messages\":[]}\n",
        )
        .unwrap();
        let r = instance_report_with_mode(
            "t1",
            Verdict::Resolved,
            &events,
            1,
            "",
            ApplyMode::Lenient,
            Some("fuzz fallback"),
        )
        .unwrap();
        assert_eq!(r.apply_mode, ApplyMode::Lenient);
        assert_eq!(r.apply_detail.as_deref(), Some("fuzz fallback"));
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["apply_mode"], "Lenient", "the report says lenient");
        std::fs::remove_dir_all(&d).ok();
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
