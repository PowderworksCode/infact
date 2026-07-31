impl ::core::convert::AsRef<str> for InfactProbe {
    fn as_ref(&self) -> &str {
        match *self {
            InfactProbe::AllowAutoMerge => "allow_auto_merge",
            InfactProbe::DeleteBranchOnMerge => "delete_branch_on_merge",
            InfactProbe::AllowUpdateBranch => "allow_update_branch",
        }
    }
}
