//! Operational knobs from the environment. `env.ts` of hedera-harness: not recipe fields, because
//! editing a recipe to shorten a timeout would be a spurious project diff. Precedence: CLI flag >
//! environment > recipe > harness default.

use std::time::Duration;

use crate::harness::spec::AgentPreset;

fn read_positive_int(name: &str) -> Option<u64> {
    let raw = std::env::var(name).ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match raw.parse::<u64>() {
        Ok(value) if value > 0 => Some(value),
        _ => {
            eprintln!(
                "[hanvil] ignoring {name}={} — expected a positive integer.",
                serde_json::to_string(raw).unwrap_or_default()
            );
            None
        }
    }
}

fn read_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// `HARNESS_MAX_ATTEMPTS`.
pub(crate) fn max_attempts() -> Option<u64> {
    read_positive_int("HARNESS_MAX_ATTEMPTS")
}

/// `HARNESS_AGENT_TIMEOUT_S`, as a duration.
pub(crate) fn agent_timeout() -> Option<Duration> {
    read_positive_int("HARNESS_AGENT_TIMEOUT_S").map(Duration::from_secs)
}

/// `HARNESS_MODEL`: the strong model.
pub(crate) fn model() -> Option<String> {
    read_string("HARNESS_MODEL")
}

/// `HARNESS_FIX_MODEL`: the repair model.
pub(crate) fn repair_model() -> Option<String> {
    read_string("HARNESS_FIX_MODEL")
}

/// `HARNESS_NO_MODEL_SWITCH=1`.
pub(crate) fn no_model_switch() -> bool {
    read_string("HARNESS_NO_MODEL_SWITCH").as_deref() == Some("1")
}

/// `HARNESS_SKILLS_REPO`.
pub(crate) fn skills_repo() -> Option<String> {
    read_string("HARNESS_SKILLS_REPO")
}

/// `HARNESS_SKILLS_REF`.
pub(crate) fn skills_ref() -> Option<String> {
    read_string("HARNESS_SKILLS_REF")
}

/// `commandAgentProvider.ts:15-24`, with one change: the `claude` preset idles for 600 s rather
/// than 90 s. A Claude `Bash` tool call — `yarn install`, a test suite — writes nothing to the
/// stream until it returns, and 90 s of that silence is a healthy agent, not a stuck one.
/// `HARNESS_AGENT_IDLE_TIMEOUT_MS` still wins.
pub(crate) fn agent_idle_timeout(preset: AgentPreset) -> Duration {
    let default = match preset {
        AgentPreset::Cursor => Duration::from_secs(90),
        AgentPreset::Claude => Duration::from_secs(600),
    };
    std::env::var("HARNESS_AGENT_IDLE_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(default, Duration::from_millis)
}
