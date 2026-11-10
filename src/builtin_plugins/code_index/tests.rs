use super::*;
use serde_json::json;

struct Fixture(PathBuf);

#[test]
#[ignore = "indexes the explicitly supplied real repository snapshot"]
fn real_snapshot_typescript_index_queries_do_not_collide() {
    let root = PathBuf::from(std::env::var("CODE_INDEX_CORPUS_ROOT").expect("set CODE_INDEX_CORPUS_ROOT"));
    let workspace = crate::tools::Workspace::new(root).unwrap();
    let result = execute(&workspace, "search", serde_json::json!({"language":"typescript","query":"agent","limit":5})).unwrap();
    assert!(result["index"]["symbols_indexed"].as_u64().unwrap() > 0);
    assert!(!result["matches"].as_array().unwrap().is_empty());
    eprintln!("CODE_INDEX_CORPUS {}", result["index"]);
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("code-index-library-{}", crate::agent_service::uuid_like()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn helper() -> u32 { 7 }\npub fn entry() -> u32 { helper() }\n").unwrap();
        std::fs::write(root.join("app.ts"), "export function helper(): number { return 7; }\nexport function entry(): number { return helper(); }\n").unwrap();
        Self(root)
    }
    fn workspace(&self) -> crate::tools::Workspace { crate::tools::Workspace::new(&self.0).unwrap() }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

#[test]
fn source_indexes_are_reused_for_both_languages_and_report_parse_omissions() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("src/broken.rs"), "pub fn broken(").unwrap();
    let workspace = fixture.workspace();
    for (language, path) in [("rust", "src/lib.rs"), ("typescript", "app.ts")] {
        let original = std::fs::read(fixture.0.join(path)).unwrap();
        assert_eq!(execute(&workspace, "status", json!({"language":language})).unwrap()["exists"], false);
        // Search must initialize the existing index without a separate build ceremony.
        let result = execute(&workspace, "search", json!({"language":language,"query":"entry","file_path":path})).unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1, "{result}");
        let source = execute(&workspace, "read", json!({"language":language,"symbol_id":matches[0]["id"]})).unwrap();
        assert!(source["content"].as_str().unwrap().contains("helper()"));
        assert_eq!(source["symbol"]["start_line"], 2);
        assert!(!source["relationship_accuracy"].as_str().unwrap().is_empty());
        let page = execute(&workspace, "list", json!({"language":language,"file_path":path,"limit":1})).unwrap();
        assert_eq!(page["symbols"].as_array().unwrap().len(), 1);
        assert_eq!(page["page"]["next_offset"], 1);
        let status = execute(&workspace, "status", json!({"language":language})).unwrap();
        assert_eq!(status["exists"], true);
        if language == "rust" {
            assert_eq!(status["index"]["unindexed_total"], 1);
            assert_eq!(status["index"]["coverage_issues"][0]["file_path"], "src/broken.rs");
        }
        assert_eq!(std::fs::read(fixture.0.join(path)).unwrap(), original);
    }
    assert!(execute(&workspace, "search", json!({"language":"unknown","query":"entry"})).unwrap_err().to_string().contains("unsupported index language"));
}

#[tokio::test]
async fn plugin_library_is_discoverable_and_routes_real_queries_in_read_only_mode() {
    use crate::plugin_runtime::{Plugin, PluginContext, PluginRuntime, ROOT};
    struct Services(Arc<crate::builtin_plugins::ToolCatalog>);
    impl Plugin for Services {
        fn id(&self) -> &str { "index-test-services" }
        fn start(&self, ctx: &PluginContext<'_>) -> Result<()> {
            ctx.provide("tools", self.0.clone())?;
            ctx.provide("skill-packages", self.0.packages.clone())
        }
    }
    let fixture = Fixture::new();
    let runtime = PluginRuntime::new();
    let catalog = Arc::new(crate::builtin_plugins::ToolCatalog::default());
    let parent = runtime.mount(ROOT, Services(catalog.clone())).unwrap();
    runtime.mount(parent, crate::skill_packages::PackagePlugin(package().unwrap())).unwrap();
    let definitions = catalog.packages.tools(&catalog.packages.enabled());
    let libraries = crate::agent_program::libraries::catalog(&definitions, &catalog.packages.catalog());
    assert_eq!(libraries["code_index"]["search"], "code_index__search");
    assert_eq!(libraries["code_index"].len(), 5);
    assert!(catalog.packages.allows("code_index__index", crate::agent_service::PermissionMode::ReadOnly, false));
    let result = catalog.execute(Arc::new(fixture.workspace()), "code_index__search", json!({"language":"typescript","query":"entry"})).await.unwrap();
    assert_eq!(result["matches"][0]["name"], "entry");
    runtime.shutdown().unwrap();
}
