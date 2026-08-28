use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RegOpsConfig {
    pub git: GitConfig,
    pub regent: RegentConfig,
    pub behavior: BehaviorConfig,
}

#[derive(Debug, Deserialize)]
pub struct GitConfig {
    pub repo: String,
    pub branch: String,
    pub local_path: String,
    pub expected_state_path: String,
}

#[derive(Debug, Deserialize)]
pub struct RegentConfig {
    pub login: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct BehaviorConfig {
    pub mode: RunningMode,
    pub interval_sec: u64,
}

#[derive(Debug, Deserialize)]
pub enum RunningMode {
    Assess,
    Enforce,
}
