#[derive(strum::AsRefStr)]
#[strum(serialize_all = "kebab-case")]
pub enum InfactProbe {
    CargoWorkspace,
    StaticSite,
    Tauri,
}
