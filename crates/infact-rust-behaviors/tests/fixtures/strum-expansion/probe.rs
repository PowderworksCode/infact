#[derive(strum::Display)]
#[strum(serialize_all = "kebab-case")]
pub enum InfactProbe {
    CargoWorkspace,
    StaticSite,
    Tauri,
}
