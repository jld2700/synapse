// Synapse app — cross-platform mobile control for the Claude Code CLI.
//
// Desktop entry point. The shared app logic and the iOS entry point live in
// the library crate (`src/lib.rs`). On iOS there is no `main` here: UIKit calls
// `synapse_ios_main` (declared in `lib.rs`) from the Objective-C app delegate.

#[cfg(not(target_os = "ios"))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    synapse_app::run_app().await
}
