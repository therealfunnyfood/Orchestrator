//! Persistent state: threads, workspaces and orchestrator sessions (state.json).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThreadRow {
    pub name: String,
    pub session_id: Option<String>,
    pub cwd: String,
    pub workspace: String,
    pub backend: String,
    pub model: String,
    pub effort: String,
    pub status: String,
    pub last_prompt: String,
    pub last_result: String,
    pub turns: u32,
    pub cost_usd: f64,
    pub created_at: f64,
    pub updated_at: f64,
    /// Last transcript entry of the session (where the next turn starts from).
    pub last_uuid: Option<String>,
    /// The session came from Claude Code; fork it on the first turn so the original stays untouched.
    pub fork_pending: bool,
    /// Permission level override ("" = the project's level).
    pub permission: String,
    /// Tools the user chose "always allow" for in this thread.
    pub always_allow: Vec<String>,
}

impl Default for ThreadRow {
    fn default() -> Self {
        ThreadRow {
            name: String::new(),
            session_id: None,
            cwd: String::new(),
            workspace: String::new(),
            backend: "plan".into(),
            model: String::new(),
            effort: String::new(),
            status: "idle".into(),
            last_prompt: String::new(),
            last_result: String::new(),
            turns: 0,
            cost_usd: 0.0,
            created_at: 0.0,
            updated_at: 0.0,
            last_uuid: None,
            fork_pending: false,
            permission: String::new(),
            always_allow: vec![],
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OrchRow {
    pub session_id: Option<String>,
    pub cost_usd: f64,
    pub turns: u32,
    pub last_uuid: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub workspace: String,
    pub recent_workspaces: Vec<String>,
    pub threads: Vec<ThreadRow>,
    /// Keyed by orchestrator key (one per workspace).
    pub orchestrators: BTreeMap<String, OrchRow>,
    /// Latest task list per conversation (thread name or orchestrator key).
    pub plans: BTreeMap<String, serde_json::Value>,
    /// Project brief and rules per workspace (written by the orchestrator).
    pub projects: BTreeMap<String, ProjectNotes>,
    /// Recent turns: (time, "plan" | "api", API-rate cost, workspace). Steers the plan/API mix.
    pub usage: Vec<(f64, String, f64, String)>,
    /// Plan/API mix per workspace (0 = all plan, 10 = all API).
    pub mixes: BTreeMap<String, u8>,
    /// Permission level per workspace.
    pub permissions: BTreeMap<String, String>,
    /// Latest plan usage limits reported by Claude Code (rate_limit_info), with when we saw it.
    pub plan_usage: Option<serde_json::Value>,
    /// One record per finished thread turn, for the orchestrator's self-tuning.
    pub efficiency: Vec<EffRecord>,
    /// Claude Code's running cost total per session id, as of its last turn.
    pub session_costs: BTreeMap<String, f64>,
    /// Version of the cost accounting the saved numbers were counted with.
    pub cost_version: u32,
    #[serde(skip)]
    path: PathBuf,
}

impl Store {
    pub fn load(path: PathBuf) -> Store {
        let mut store: Store = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        store.path = path;
        if store.cost_version < 1 {
            // Before 0.4.4 every resumed turn counted the session's whole running total
            // again, so saved costs were far too high. Start them over.
            store.usage.clear();
            store.efficiency.clear();
            for t in &mut store.threads {
                t.cost_usd = 0.0;
            }
            for o in store.orchestrators.values_mut() {
                o.cost_usd = 0.0;
            }
            store.cost_version = 1;
            store.save();
        }
        // Anything mid-turn when the app last closed was cut off.
        for t in &mut store.threads {
            if t.status == "running" || t.status == "queued" {
                t.status = "interrupted".into();
            }
        }
        store
    }

    pub fn save(&self) {
        let tmp = self.path.with_extension("json.tmp");
        if let Ok(text) = serde_json::to_string_pretty(self) {
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&ThreadRow> {
        self.threads.iter().find(|t| t.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut ThreadRow> {
        self.threads.iter_mut().find(|t| t.name == name)
    }

    pub fn in_workspace(&self, ws: &str) -> Vec<ThreadRow> {
        self.threads.iter().filter(|t| same_path(&t.workspace, ws)).cloned().collect()
    }

    pub fn remember_workspace(&mut self, ws: &str) {
        self.recent_workspaces.retain(|p| !same_path(p, ws) && Path::new(p).is_dir());
        self.recent_workspaces.insert(0, ws.to_string());
        self.recent_workspaces.truncate(12);
    }
}

pub fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(b.trim_end_matches(['\\', '/']))
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectNotes {
    pub brief: String,
    pub rules: String,
    /// The orchestrator's own lessons about which model/effort fits which work.
    pub tuning: String,
    pub imported_at: Option<f64>,
    pub updated_at: f64,
}

impl Store {
    pub fn notes(&self, ws: &str) -> Option<&ProjectNotes> {
        self.projects.iter().find(|(k, _)| same_path(k, ws)).map(|(_, v)| v)
    }

    pub fn notes_mut(&mut self, ws: &str) -> &mut ProjectNotes {
        let key = self.projects.keys().find(|k| same_path(k, ws)).cloned().unwrap_or_else(|| ws.to_string());
        self.projects.entry(key).or_default()
    }
}

pub const MIX_WINDOW_SECS: f64 = 24.0 * 3600.0;

impl Store {
    /// Remember a session's running total for its next turn (and forks of it).
    pub fn note_session_cost(&mut self, session: Option<&str>, total: f64) {
        if let Some(s) = session {
            self.session_costs.insert(s.to_string(), total);
        }
    }

    pub fn session_cost(&self, session: Option<&str>) -> Option<f64> {
        session.and_then(|s| self.session_costs.get(s).copied())
    }

    /// Record a finished turn's cost; the part that ran on the plan before an auto
    /// fallback counts as plan.
    pub fn record_turn_usage(&mut self, r: &crate::runner::TurnResult) {
        if r.plan_cost > 0.0 {
            self.record_usage("plan", r.plan_cost);
        }
        self.record_usage(&r.backend_used, r.cost_usd - r.plan_cost);
        self.note_session_cost(r.session_id.as_deref(), r.session_total);
    }

    pub fn record_usage(&mut self, backend: &str, cost: f64) {
        let t = now();
        let ws = self.workspace.clone();
        self.usage.retain(|(ts, ..)| t - ts < 7.0 * 86400.0);
        self.usage.push((t, backend.to_string(), cost.max(0.0), ws));
    }

    /// (plan cost, api cost) for the current workspace over the mix window.
    pub fn usage_split(&self) -> (f64, f64) {
        let t = now();
        self.usage
            .iter()
            .filter(|(ts, _, _, ws)| t - ts < MIX_WINDOW_SECS && same_path(ws, &self.workspace))
            .fold((0.0, 0.0), |(p, a), (_, b, c, _)| if b == "api" { (p, a + c) } else { (p + c, a) })
    }

    /// This project's plan/API mix, or `default` if it hasn't been set.
    pub fn mix(&self, default: u8) -> u8 {
        self.mixes.iter().find(|(k, _)| same_path(k, &self.workspace)).map(|(_, v)| *v).unwrap_or(default)
    }

    pub fn permission(&self, default: &str) -> String {
        self.permissions.iter().find(|(k, _)| same_path(k, &self.workspace)).map(|(_, v)| v.clone()).unwrap_or_else(|| default.to_string())
    }

    pub fn set_permission(&mut self, value: &str) {
        let ws = self.workspace.clone();
        self.permissions.retain(|k, _| !same_path(k, &ws));
        self.permissions.insert(ws, value.to_string());
    }

    pub fn set_mix(&mut self, value: u8) {
        let ws = self.workspace.clone();
        self.mixes.retain(|k, _| !same_path(k, &ws));
        self.mixes.insert(ws, value.min(10));
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EffRecord {
    pub ts: f64,
    pub workspace: String,
    pub thread: String,
    pub task: String,
    pub model: String,
    pub effort: String,
    pub steps: u64,
    pub duration_ms: u64,
    pub cost: f64,
    pub ok: bool,
    /// The orchestrator's verdict: right | too_weak | too_strong.
    pub verdict: String,
    pub note: String,
}

impl Store {
    pub fn record_efficiency(&mut self, mut r: EffRecord) {
        r.ts = now();
        r.workspace = self.workspace.clone();
        self.efficiency.push(r);
        let len = self.efficiency.len();
        if len > 3000 {
            self.efficiency.drain(..len - 3000);
        }
    }
}
