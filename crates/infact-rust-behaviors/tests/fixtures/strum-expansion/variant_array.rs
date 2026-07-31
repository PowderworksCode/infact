impl ::strum::VariantArray for InfactProbe {
    const VARIANTS: &'static [Self] = &[
        InfactProbe::RepositoryShape,
        InfactProbe::Documentation,
        InfactProbe::GithubSafeguards,
    ];
}
