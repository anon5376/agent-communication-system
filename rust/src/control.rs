//! Pausing agents and giving them budgets.
//!
//! Both live in `agents.meta_json`, so the TypeScript and Rust implementations
//! share them on one bus.db without a schema change (mirror: src/core/control.ts):
//!
//!   "paused": {"atMs": 1700000000000, "by": "operator", "reason": "lunch"}
//!   "budget": {"turns": 20, "minutes": 60, "usd": 2.5, "setMs": 1700000000000,
//!              "base": {"turns": 3, "minutes": 1.5, "usd": 0.1}}
//!
//! A budget counts what the agent's supervisor records in `sessions/<agent>.json`
//! since the budget was set (or the agent last resumed): turns, minutes the CLI
//! ran, and the cost the CLI itself reported. A CLI that reports no cost counts
//! as 0 dollars, so only turns and minutes are dependable for every CLI.

use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// What one agent has used, as its supervisor recorded it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub turns: f64,
    pub minutes: f64,
    pub usd: f64,
}

impl Usage {
    pub fn to_json(self) -> Value {
        json!({"turns": self.turns, "minutes": round2(self.minutes), "usd": round4(self.usd)})
    }
    fn from_json(v: &Value) -> Usage {
        Usage {
            turns: v["turns"].as_f64().unwrap_or(0.0),
            minutes: v["minutes"].as_f64().unwrap_or(0.0),
            usd: v["usd"].as_f64().unwrap_or(0.0),
        }
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}
fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// The running totals in `<home>/sessions/<agent>.json` (zero when there is none yet).
pub fn session_usage(home: &Path, agent_id: &str) -> Usage {
    let v: Value = fs::read_to_string(home.join("sessions").join(format!("{agent_id}.json")))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    Usage {
        turns: v["turns"].as_f64().unwrap_or(0.0),
        minutes: v["latencyMs"].as_f64().unwrap_or(0.0) / 60_000.0,
        usd: v["costUSD"].as_f64().unwrap_or(0.0),
    }
}

/// Limits for a budget. At least one must be set.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Limits {
    pub turns: Option<f64>,
    pub minutes: Option<f64>,
    pub usd: Option<f64>,
}

impl Limits {
    pub fn is_empty(&self) -> bool {
        self.turns.is_none() && self.minutes.is_none() && self.usd.is_none()
    }

    /// "20 turns, 60 min, $2.00", only the limits that are set.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(t) = self.turns {
            parts.push(format!("{} turns", short(t)));
        }
        if let Some(m) = self.minutes {
            parts.push(format!("{} min", short(m)));
        }
        if let Some(u) = self.usd {
            parts.push(format!("${u:.2}"));
        }
        parts.join(", ")
    }
}

/// Who paused an agent, when, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Paused {
    pub at_ms: i64,
    pub by: String,
    pub reason: String,
}

pub fn paused(meta: &Value) -> Option<Paused> {
    let p = meta.get("paused")?;
    if !p.is_object() {
        return None;
    }
    Some(Paused {
        at_ms: p["atMs"].as_i64().unwrap_or(0),
        by: p["by"].as_str().unwrap_or("").to_string(),
        reason: p["reason"].as_str().unwrap_or("").to_string(),
    })
}

/// A budget with what has been used against it.
#[derive(Debug, Clone, PartialEq)]
pub struct Budget {
    pub limits: Limits,
    pub used: Usage,
    pub set_ms: i64,
}

impl Budget {
    /// The first limit reached, in words ("20 of 20 turns"), or None.
    pub fn over(&self) -> Option<String> {
        let checks = [
            (self.limits.turns, self.used.turns, "turns"),
            (self.limits.minutes, self.used.minutes, "minutes"),
            (self.limits.usd, self.used.usd, "dollars reported"),
        ];
        checks.iter().find_map(|(limit, used, unit)| {
            limit
                .filter(|l| used >= l)
                .map(|l| format!("{} of {} {unit}", short(*used), short(l)))
        })
    }

    /// "4/20 turns  12/60 min  $0.30/$2.00", only the limits that are set.
    pub fn line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(l) = self.limits.turns {
            parts.push(format!("{}/{} turns", short(self.used.turns), short(l)));
        }
        if let Some(l) = self.limits.minutes {
            parts.push(format!("{}/{} min", short(self.used.minutes), short(l)));
        }
        if let Some(l) = self.limits.usd {
            parts.push(format!("${:.2}/${:.2}", self.used.usd, l));
        }
        parts.join("  ")
    }
}

/// A number to one decimal, without a trailing ".0".
pub fn short(x: f64) -> String {
    let x = (x * 10.0).round() / 10.0;
    if x.fract() == 0.0 {
        format!("{}", x as i64)
    } else {
        format!("{x:.1}")
    }
}

/// The agent's budget and what it has used since the budget was set, given its current totals.
pub fn budget(meta: &Value, now: Usage) -> Option<Budget> {
    let b = meta.get("budget")?;
    if !b.is_object() {
        return None;
    }
    let limits = Limits {
        turns: b["turns"].as_f64(),
        minutes: b["minutes"].as_f64(),
        usd: b["usd"].as_f64(),
    };
    if limits.is_empty() {
        return None;
    }
    let base = Usage::from_json(&b["base"]);
    Some(Budget {
        limits,
        used: Usage {
            turns: (now.turns - base.turns).max(0.0),
            minutes: (now.minutes - base.minutes).max(0.0),
            usd: (now.usd - base.usd).max(0.0),
        },
        set_ms: b["setMs"].as_i64().unwrap_or(0),
    })
}

/// The JSON stored for a budget that starts counting from `base`.
pub fn budget_json(limits: Limits, base: Usage, now_ms: i64) -> Value {
    let mut b = json!({"setMs": now_ms, "base": base.to_json()});
    if let Some(t) = limits.turns {
        b["turns"] = json!(t);
    }
    if let Some(m) = limits.minutes {
        b["minutes"] = json!(m);
    }
    if let Some(u) = limits.usd {
        b["usd"] = json!(u);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_budget_counts_from_its_base_and_names_the_first_limit_reached() {
        let limits = Limits {
            turns: Some(3.0),
            minutes: Some(10.0),
            usd: None,
        };
        let base = Usage {
            turns: 5.0,
            minutes: 2.0,
            usd: 0.0,
        };
        let meta = json!({ "budget": budget_json(limits, base, 1) });
        let b = budget(
            &meta,
            Usage {
                turns: 7.0,
                minutes: 4.5,
                usd: 0.0,
            },
        )
        .unwrap();
        assert_eq!(b.used.turns, 2.0);
        assert_eq!(b.over(), None);
        assert_eq!(b.line(), "2/3 turns  2.5/10 min");
        let b = budget(
            &meta,
            Usage {
                turns: 8.0,
                minutes: 4.5,
                usd: 0.0,
            },
        )
        .unwrap();
        assert_eq!(b.over().as_deref(), Some("3 of 3 turns"));
    }
}
