#[derive(strum::AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum InfactProbe {
    AllowAutoMerge,
    DeleteBranchOnMerge,
    AllowUpdateBranch,
}
