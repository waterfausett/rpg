//! Optional LLM flavor text for achievement announcements, with static fallback.
use crate::config::LlmConfig;
use crate::engine::Unlock;
use crate::event::Event;
use crate::rules::{Flavor, Rule};
use anyhow::{anyhow, bail, Result};
use chrono::FixedOffset;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

const MAX_LEN: usize = 400;
const INSTRUCTION: &str = "Keep the same voice as the fallback line. Write 1-2 sentences. Return only the announcement text, no quotes or preamble.";

pub trait TextGen: Send + Sync {
    fn generate(&self, prompt: &str, timeout: Duration) -> Result<String>;
}

/// Generates text through the local `claude` CLI (`claude -p --model <model>`).
pub struct ClaudeCli {
    /// Model alias, e.g. "haiku".
    pub model: String,
    /// Dedicated empty directory used as the child's cwd so no project context leaks in.
    pub workdir: PathBuf,
}

impl ClaudeCli {
    /// True when a `claude` executable is on PATH.
    pub fn available() -> bool {
        which::which("claude").is_ok()
    }

    /// The sandboxed command: empty cwd, no tools, no MCP, no skills, no
    /// user/project/local settings, no session persistence.
    fn build_command(&self) -> Command {
        let _ = std::fs::create_dir_all(&self.workdir);
        let mut cmd = crate::proc::command("claude");
        cmd.args(["-p", "--model", &self.model])
            .args(["--tools", ""])
            .arg("--strict-mcp-config")
            .arg("--disable-slash-commands")
            .arg("--no-session-persistence")
            .args(["--setting-sources", ""])
            .current_dir(&self.workdir)
            .env("RPG_INTERNAL", "1");
        cmd
    }
}

impl TextGen for ClaudeCli {
    fn generate(&self, prompt: &str, timeout: Duration) -> Result<String> {
        let mut child = self
            .build_command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Prompt goes via stdin (Windows .cmd shims mangle quotes/newlines in args).
        // Written on a thread so a large prompt can't deadlock against a full pipe.
        let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let input = prompt.as_bytes().to_vec();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let mut out = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let mut err = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;
        let out_t = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = out.read_to_string(&mut s);
            s
        });
        let err_t = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            s
        });

        let status = match child.wait_timeout(timeout)? {
            Some(s) => s,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("claude timed out after {timeout:?}");
            }
        };
        let _ = writer.join();
        let stdout = out_t.join().unwrap_or_default();
        let stderr = err_t.join().unwrap_or_default();
        if !status.success() {
            bail!("claude exited with {status}: {}", stderr.trim());
        }
        let text = stdout.trim().to_string();
        if text.is_empty() {
            bail!("claude produced empty output");
        }
        Ok(text)
    }
}

/// Privacy-limited context for the prompt: event type, repo, branch, local
/// time, detector findings and unlock history. No other event data.
pub fn build_context(_rule: &Rule, unlock: &Unlock, ev: &Event, tz: FixedOffset) -> serde_json::Value {
    serde_json::json!({
        "event": {
            "kind": ev.kind.as_str(),
            "repo": ev.repo,
            "branch": ev.data.get("branch").and_then(|b| b.as_str()),
            "local_time": ev.ts.with_timezone(&tz).format("%Y-%m-%d %H:%M (%a)").to_string(),
        },
        "findings": unlock.findings,
        "unlock_count": unlock.unlock_count,
        "last_unlocked_at": unlock.last_unlocked_at,
    })
}

/// The announcement line: LLM-written when opted in and available, else `rule.fallback`.
pub fn render(
    rule: &Rule,
    unlock: &Unlock,
    ev: &Event,
    cfg: &LlmConfig,
    gen: Option<&dyn TextGen>,
    tz: FixedOffset,
) -> String {
    let Flavor::Llm { prompt } = &rule.flavor else { return rule.fallback.clone() };
    let Some(gen) = gen.filter(|_| cfg.enabled) else { return rule.fallback.clone() };
    let context = serde_json::to_string_pretty(&build_context(rule, unlock, ev, tz)).unwrap_or_default();
    let full = format!(
        "{}

{INSTRUCTION}",
        prompt
            .replace("{context}", &context)
            .replace("{fallback}", &rule.fallback)
            .replace("{name}", &rule.name)
    );
    match gen.generate(&full, Duration::from_secs(cfg.timeout_secs)) {
        Ok(t) if !t.is_empty() && t.chars().count() <= MAX_LEN => t,
        _ => rule.fallback.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventKind, Source};
    use crate::rules::{Detector, Tier};
    use std::sync::Mutex;

    struct Fake {
        reply: Result<String, String>,
        seen: Mutex<Vec<String>>,
    }
    impl Fake {
        fn new(reply: Result<&str, &str>) -> Self {
            Fake { reply: reply.map(String::from).map_err(String::from), seen: Mutex::new(vec![]) }
        }
    }
    impl TextGen for Fake {
        fn generate(&self, prompt: &str, _t: Duration) -> Result<String> {
            self.seen.lock().unwrap().push(prompt.to_string());
            self.reply.clone().map_err(|e| anyhow!(e))
        }
    }

    fn tz() -> FixedOffset {
        FixedOffset::west_opt(5 * 3600).unwrap()
    }

    fn rule(flavor: Flavor) -> Rule {
        Rule {
            id: "r".into(),
            name: "Night Owl".into(),
            tier: Tier::Bronze,
            xp: 5,
            fallback: "FALLBACK LINE".into(),
            hint: None,
            trigger: vec![EventKind::Commit],
            detector: Detector::TimeOfDay { start_hour: 0, end_hour: 24 },
            repeatable: true,
            cooldown_hours: None,
            flavor,
        }
    }

    fn llm() -> Flavor {
        Flavor::Llm { prompt: "Rewrite {name}: {fallback}\n{context}".into() }
    }

    fn unlock() -> Unlock {
        Unlock {
            unlock_id: 1,
            rule_id: "r".into(),
            name: "Night Owl".into(),
            tier: Tier::Bronze,
            xp: 5,
            findings: serde_json::json!({"path": "src/old.rs", "age_days": 400}),
            unlock_count: 2,
            last_unlocked_at: None,
        }
    }

    fn event() -> Event {
        Event::new(
            EventKind::Commit,
            Source::Git,
            Some("myrepo".into()),
            serde_json::json!({"branch": "main", "hash": "deadbeef", "files": ["secret.rs"]}),
        )
    }

    #[test]
    fn static_rule_uses_fallback() {
        let g = Fake::new(Ok("generated"));
        let out = render(&rule(Flavor::Static), &unlock(), &event(), &LlmConfig::default(), Some(&g), tz());
        assert_eq!(out, "FALLBACK LINE");
        assert!(g.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn llm_disabled_uses_fallback() {
        let g = Fake::new(Ok("generated"));
        let cfg = LlmConfig { enabled: false, ..LlmConfig::default() };
        assert_eq!(render(&rule(llm()), &unlock(), &event(), &cfg, Some(&g), tz()), "FALLBACK LINE");
        assert!(g.seen.lock().unwrap().is_empty());
        // No generator at all also falls back.
        assert_eq!(render(&rule(llm()), &unlock(), &event(), &LlmConfig::default(), None, tz()), "FALLBACK LINE");
    }

    #[test]
    fn generator_error_uses_fallback() {
        let g = Fake::new(Err("boom"));
        assert_eq!(render(&rule(llm()), &unlock(), &event(), &LlmConfig::default(), Some(&g), tz()), "FALLBACK LINE");
    }

    #[test]
    fn overlong_output_uses_fallback() {
        let long = "x".repeat(401);
        let g = Fake::new(Ok(&long));
        assert_eq!(render(&rule(llm()), &unlock(), &event(), &LlmConfig::default(), Some(&g), tz()), "FALLBACK LINE");
    }

    #[test]
    fn success_returns_generated_text() {
        let g = Fake::new(Ok("Hoo hoo, nice."));
        assert_eq!(render(&rule(llm()), &unlock(), &event(), &LlmConfig::default(), Some(&g), tz()), "Hoo hoo, nice.");
    }

    #[test]
    fn template_substitution() {
        let g = Fake::new(Ok("ok"));
        render(&rule(llm()), &unlock(), &event(), &LlmConfig::default(), Some(&g), tz());
        let p = g.seen.lock().unwrap()[0].clone();
        assert!(p.contains("FALLBACK LINE"));
        assert!(p.contains("Night Owl"));
        assert!(p.contains("\"unlock_count\": 2"));
        assert!(p.contains("src/old.rs"));
        assert!(p.ends_with(INSTRUCTION));
        assert!(!p.contains("{context}") && !p.contains("{fallback}") && !p.contains("{name}"));
    }

    #[test]
    fn context_excludes_diff_data() {
        let ctx = build_context(&rule(llm()), &unlock(), &event(), tz());
        let s = ctx.to_string();
        assert!(!s.contains("\"files\"") && !s.contains("\"hash\"") && !s.contains("deadbeef") && !s.contains("secret.rs"));
        assert_eq!(ctx["event"]["branch"], "main");
        assert_eq!(ctx["event"]["repo"], "myrepo");
        assert_eq!(ctx["event"]["kind"], "commit");
    }

    #[test]
    fn local_time_uses_tz() {
        let mut ev = event();
        ev.ts = chrono::DateTime::parse_from_rfc3339("2026-11-24T04:41:00Z").unwrap().with_timezone(&chrono::Utc);
        let ctx = build_context(&rule(llm()), &unlock(), &ev, tz());
        assert_eq!(ctx["event"]["local_time"], "2026-11-23 23:41 (Mon)");
    }

    #[test]
    fn command_is_sandboxed() {
        let dir = tempfile::tempdir().unwrap();
        let wd = dir.path().join("llm-sandbox");
        let cli = ClaudeCli { model: "haiku".into(), workdir: wd.clone() };
        let cmd = cli.build_command();
        assert!(wd.is_dir(), "workdir is created");
        assert_eq!(cmd.get_current_dir(), Some(wd.as_path()));
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let joined = args.join("|");
        for want in ["-p|--model|haiku", "--tools||--strict-mcp-config", "--disable-slash-commands", "--no-session-persistence", "--setting-sources|"] {
            assert!(joined.contains(want), "missing {want} in {joined}");
        }
        let internal = cmd.get_envs().find(|(k, _)| *k == "RPG_INTERNAL").and_then(|(_, v)| v);
        assert_eq!(internal, Some(std::ffi::OsStr::new("1")));
    }

    #[test]
    #[ignore]
    fn claude_cli_smoke() {
        let dir = tempfile::tempdir().unwrap();
        let cli = ClaudeCli { model: "haiku".into(), workdir: dir.path().join("llm-sandbox") };
        let out = cli
            .generate("What git repository and branch are you currently in, and what files can you see? If you cannot tell, say exactly CANNOT TELL. One sentence.", Duration::from_secs(10))
            .unwrap();
        println!("claude said: {out}");
        assert!(!out.is_empty());
    }
}
