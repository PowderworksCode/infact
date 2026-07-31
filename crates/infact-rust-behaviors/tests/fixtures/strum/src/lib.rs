#[derive(Debug, Clone, Copy)]
pub enum ProjectCapability {
    CargoWorkspace,
    StaticSite,
    Tauri,
}

impl ProjectCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CargoWorkspace => "cargo-workspace",
            Self::StaticSite => "static-site",
            Self::Tauri => "tauri",
        }
    }
}

impl std::fmt::Display for ProjectCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(formatter)
    }
}

pub enum NotExhaustive {
    FirstValue,
    SecondValue,
}

impl NotExhaustive {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstValue => "first-value",
            Self::SecondValue => "custom",
        }
    }
}

impl std::fmt::Display for NotExhaustive {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(formatter)
    }
}

pub enum GithubSetting {
    AllowAutoMerge,
    DeleteBranchOnMerge,
    AllowUpdateBranch,
}

impl GithubSetting {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllowAutoMerge => "allow_auto_merge",
            Self::DeleteBranchOnMerge => "delete_branch_on_merge",
            Self::AllowUpdateBranch => "allow_update_branch",
        }
    }
}

pub enum CheckCategory {
    RepositoryShape,
    Documentation,
    GithubSafeguards,
}

impl CheckCategory {
    pub const ALL: [Self; 3] = [
        Self::RepositoryShape,
        Self::Documentation,
        Self::GithubSafeguards,
    ];
}
