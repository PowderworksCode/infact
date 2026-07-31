impl ::core::fmt::Display for InfactProbe {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        match *self {
            InfactProbe::CargoWorkspace => ::core::fmt::Display::fmt("cargo-workspace", f),
            InfactProbe::StaticSite => ::core::fmt::Display::fmt("static-site", f),
            InfactProbe::Tauri => ::core::fmt::Display::fmt("tauri", f),
        }
    }
}
