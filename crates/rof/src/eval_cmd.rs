//! `rof eval` over the frozen task slice: winnability pre-flight plus the
//! Docker-engine run and JSON report.

use std::path::{Path, PathBuf};

pub(crate) const EVAL_USAGE: &str = "usage: rof eval --tasks-dir DIR [--ids a,b] [--out-dir DIR] [--report PATH] [--winnability-only]";

#[derive(Debug, PartialEq)]
pub(crate) struct EvalArgs {
    pub tasks_dir: PathBuf,
    pub ids: Vec<String>,
    pub out_dir: PathBuf,
    pub report: Option<PathBuf>,
    pub winnability_only: bool,
}

pub(crate) fn parse_eval(argv: &[String]) -> Result<EvalArgs, String> {
    let a: Vec<&str> = argv.iter().map(String::as_str).collect();
    if a.len() < 2 || a[1] != "eval" {
        return Err(EVAL_USAGE.into());
    }
    let (mut tasks_dir, mut ids, mut out_dir, mut report) = (None, Vec::new(), None, None);
    let mut winnability_only = false;
    let mut i = 2;
    while i < a.len() {
        let flag = a[i];
        let (k, inline) = match flag.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (flag, None),
        };
        let take = |i: &mut usize, inline: Option<String>| -> Result<String, String> {
            if let Some(v) = inline {
                return Ok(v);
            }
            *i += 1;
            a.get(*i)
                .map(|s| (*s).to_string())
                .ok_or_else(|| format!("{k} needs a value"))
        };
        match k {
            "--tasks-dir" => tasks_dir = Some(take(&mut i, inline)?),
            "--ids" => {
                ids = take(&mut i, inline)?
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            "--out-dir" => out_dir = Some(take(&mut i, inline)?),
            "--report" => report = Some(take(&mut i, inline)?),
            "--winnability-only" => {
                winnability_only = true;
            }
            _ => return Err(EVAL_USAGE.into()),
        }
        i += 1;
    }
    Ok(EvalArgs {
        tasks_dir: PathBuf::from(tasks_dir.ok_or("missing --tasks-dir")?),
        ids,
        out_dir: PathBuf::from(out_dir.unwrap_or_else(|| "/tmp/rof-eval-out".into())),
        report: report.map(PathBuf::from),
        winnability_only,
    })
}

fn eval_winnability_caps() -> eval::ToolCaps {
    eval::ToolCaps {
        can_create: true,
        can_patch: true,
        exec_prefixes: Vec::new(),
    }
}

fn eval_verdict_from_workdir(workdir: &Path) -> eval::Verdict {
    for name in ["reward.txt", "reward.json"] {
        let p = workdir.join(name);
        if p.is_file() {
            if let Ok(v) = eval::check_tb_reward(&p) {
                return v;
            }
        }
    }
    eval::Verdict::ErrorNoReport
}

pub(crate) async fn run_eval(args: &EvalArgs) -> i32 {
    let instances = match eval::load_tb_slice(&args.tasks_dir, &args.ids) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cannot load slice: {e}");
            return 2;
        }
    };
    let _excluded = eval::preflight(&instances);
    let caps = eval_winnability_caps();
    let engine = eval::DockerEngine::new();
    let mut reports = Vec::new();
    for inst in &instances {
        let task_dir = args.tasks_dir.join(&inst.id);
        for bad in eval::check_winnability(&task_dir, &caps) {
            eprintln!("unwinnable {}: {bad}", inst.id);
        }
        if args.winnability_only {
            continue;
        }
        let work = args.out_dir.join(&inst.id);
        if std::fs::create_dir_all(&work).is_err() {
            eprintln!("cannot create {}", work.display());
            return 2;
        }
        let start = std::time::Instant::now();
        let verdict = match eval::Engine::run_instance(&engine, inst, &work) {
            Ok(outcome) => {
                if let Some(hit) = outcome.reward {
                    if hit {
                        eval::Verdict::Resolved
                    } else {
                        eval::Verdict::Unresolved
                    }
                } else {
                    eval_verdict_from_workdir(&work)
                }
            }
            Err(e) => {
                eprintln!("infra {}: {e}", inst.id);
                eval::Verdict::InfraFailure
            }
        };
        let wall = start.elapsed().as_secs();
        let events_path = work.join("events.jsonl");
        let _ = std::fs::write(&events_path, "");
        match eval::instance_report(&inst.id, verdict, &events_path, wall, "") {
            Ok(r) => reports.push(r),
            Err(e) => {
                eprintln!("cannot report {}: {e}", inst.id);
                return 2;
            }
        }
    }
    let report = eval::RunReport { instances: reports };
    if let Some(path) = args.report.as_deref() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("cannot write {}: {e}", path.display());
                return 2;
            }
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::argv;

    #[test]
    fn cli_eval_parses_tasks_dir_and_report() {
        let a = parse_eval(&argv(&[
            "rof",
            "eval",
            "--tasks-dir",
            "/tmp/tasks",
            "--ids",
            "aaa,bbb",
            "--out-dir",
            "/tmp/eval-out",
            "--report",
            "/tmp/eval-out/report.json",
        ]))
        .unwrap();
        assert_eq!(a.tasks_dir, PathBuf::from("/tmp/tasks"));
        assert_eq!(a.ids, vec!["aaa".to_string(), "bbb".to_string()]);
        assert_eq!(a.report, Some(PathBuf::from("/tmp/eval-out/report.json")));
        assert!(!a.winnability_only);
    }

    #[tokio::test]
    async fn eval_winnability_only_reports_violations_without_docker() {
        let root = std::env::temp_dir().join(format!("rof-eval-test-{}", std::process::id()));
        let tasks = root.join("tasks");
        std::fs::create_dir_all(tasks.join("aaa").join("solution")).unwrap();
        std::fs::create_dir_all(tasks.join("aaa").join("tests")).unwrap();
        std::fs::write(
            tasks.join("aaa").join("task.toml"),
            "image = \"img\"\ntimeout = 60\n",
        )
        .unwrap();
        std::fs::write(tasks.join("aaa").join("instruction.md"), "# aaa\nDo it.\n").unwrap();
        std::fs::write(
            tasks.join("aaa").join("solution").join("solve.sh"),
            "#!/bin/sh\necho ok\n",
        )
        .unwrap();
        std::fs::write(
            tasks.join("aaa").join("tests").join("test.sh"),
            "#!/bin/sh\nexit 0\n",
        )
        .unwrap();
        let args = EvalArgs {
            tasks_dir: tasks.clone(),
            ids: vec![],
            out_dir: root.join("out"),
            report: Some(root.join("out").join("report.json")),
            winnability_only: true,
        };
        let code = run_eval(&args).await;
        assert_eq!(code, 0);
        let raw = std::fs::read_to_string(args.report.unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v.get("instances").and_then(|x| x.as_array()).is_some());
        std::fs::remove_dir_all(&root).ok();
    }
}
