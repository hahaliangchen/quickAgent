//! Static metadata for built-in host tools. Package tools stay in the dynamic catalog.

use crate::{runtime::agent_program::libraries::Libraries, server::agent_service::PermissionMode};

/// Registration groups, independent of the JS library namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatalogKind {
    File,
    Memory,
    Process,
    Verify,
    Plan,
    Git,
    Http,
    Browser,
    Batch,
    Dedicated,
}

#[derive(Clone, Copy)]
enum AllowPolicy {
    All,
    WorkspaceWrite,
    LinuxWorkspaceExecution,
    Interactive,
}

impl AllowPolicy {
    fn allows(self, mode: PermissionMode) -> bool {
        use PermissionMode::*;

        match (self, mode) {
            (_, Ask | AutoReview | FullAccess) | (Self::All, _) => true,
            (Self::WorkspaceWrite, WorkspaceWrite) => true,
            (Self::LinuxWorkspaceExecution, WorkspaceWrite) => cfg!(target_os = "linux"),
            _ => false,
        }
    }
}

pub(crate) struct ToolSpec {
    pub name: &'static str,
    pub library: &'static str,
    pub method: &'static str,
    pub catalog: CatalogKind,
    allow: AllowPolicy,
    fallback_visible: bool,
    pub tracks_mutation: bool,
    pub needs_approval_in_ask: bool,
    pub records_written_path: bool,
    pub write_summary_label: Option<&'static str>,
}

impl ToolSpec {
    const fn new(
        name: &'static str,
        library: &'static str,
        method: &'static str,
        catalog: CatalogKind,
    ) -> Self {
        Self {
            name,
            library,
            method,
            catalog,
            allow: AllowPolicy::All,
            fallback_visible: !matches!(catalog, CatalogKind::Dedicated),
            tracks_mutation: false,
            needs_approval_in_ask: false,
            records_written_path: false,
            write_summary_label: None,
        }
    }

    const fn workspace_write(mut self) -> Self {
        self.allow = AllowPolicy::WorkspaceWrite;
        self
    }

    const fn linux_workspace_execution(mut self) -> Self {
        self.allow = AllowPolicy::LinuxWorkspaceExecution;
        self.approval()
    }

    const fn interactive(mut self) -> Self {
        self.allow = AllowPolicy::Interactive;
        self.approval()
    }

    const fn approval(mut self) -> Self {
        self.needs_approval_in_ask = true;
        self
    }

    const fn tracked(mut self) -> Self {
        self.tracks_mutation = true;
        self
    }

    const fn written_path(mut self, label: &'static str) -> Self {
        self.records_written_path = true;
        self.write_summary_label = Some(label);
        self
    }

    const fn fallback_visible(mut self) -> Self {
        self.fallback_visible = true;
        self
    }
}

macro_rules! tool_specs {
    ($($catalog:ident {
        $($lib:ident.$method:ident $(=> $name:ident)? $(, $policy:ident $(($arg:literal))?)*;)*
    })*) => {
        &[$($(ToolSpec::new(tool_specs!(@name $method $(, $name)?),
            stringify!($lib), stringify!($method), CatalogKind::$catalog)
            $(.$policy($($arg)?))*,)*)*]
    };
    (@name $method:ident) => { stringify!($method) };
    (@name $method:ident, $name:ident) => { stringify!($name) };
}

const SPECS: &[ToolSpec] = tool_specs! {
    File {
        // Managed shell sessions track their own changes. Killing one never needs approval.
        process.exec_command, approval;
        process.write_stdin, approval;
        process.kill_session;
        io.workspace_info;
        git.status => git_status;
        git.diff => git_diff;
        git.log => git_log;
        io.list => list_dir;
        io.read => read_file;
        io.read_lines => read_file_lines;
        io.read_chunk => read_file_chunk;
        io.search => search_text;
        io.write => write_file, workspace_write, tracked, written_path("已写入");
        io.replace => replace_range, workspace_write, tracked, written_path("已写入");
        io.edit => edit_file, workspace_write, tracked, written_path("已写入");
        io.delete => delete_path, workspace_write, tracked, written_path("已删除");
        io.rename => rename_path, workspace_write, tracked, written_path("已移动");
        io.mkdir => create_dir, workspace_write, tracked, written_path("已创建目录");
    }
    Memory {
        memory.record_work_memory;
        memory.list_work_memory;
        memory.search_work_memory;
        memory.record_architecture_memory;
        memory.list_architecture_memory;
        memory.search_architecture_memory;
        memory.record_symbol_business_context;
        memory.list_symbol_business_context;
        memory.search_symbol_business_context;
    }
    Process {
        process.install_dependencies, linux_workspace_execution, tracked;
        process.run_project_script, linux_workspace_execution, tracked;
        process.get_project_process;
        process.stop_project_process, linux_workspace_execution;
        process.run_program, linux_workspace_execution, tracked;
    }
    Verify {
        verify.check => verify_check, workspace_write, approval, tracked;
        verify.test => verify_test, workspace_write, approval, tracked;
    }
    Plan {
        plan.set => plan_set;
        plan.update => plan_update;
    }
    Git {
        git.blame => git_blame;
        git.commit => git_commit, workspace_write, approval, tracked;
        git.restore => git_restore, workspace_write, approval, tracked;
    }
    Http {
        http.probe => http_probe;
    }
    Browser {
        browser.open => browser_open, interactive;
        browser.read => browser_read, interactive;
        browser.wait => browser_wait, interactive;
        browser.diagnostics => browser_diagnostics, interactive;
        browser.network => browser_network, interactive;
        browser.tabs => browser_tabs, interactive;
        browser.switch_tab => browser_switch_tab, interactive;
        browser.hover => browser_hover, interactive;
        browser.fill => browser_fill, interactive;
        browser.set_viewport => browser_set_viewport, interactive;
        browser.scroll => browser_scroll, interactive;
        browser.click => browser_click, interactive;
        browser.press_key => browser_press_key, interactive;
        browser.upload => browser_upload, interactive;
        browser.screenshot => browser_screenshot, interactive;
        browser.close => browser_close, interactive;
    }
    Batch {
        io.apply_edits, workspace_write, tracked;
    }
    Dedicated {
        process.run_readonly => run_readonly_command;
        vision.view_image, fallback_visible;
        vision.analyze_images;
        vision.image_analysis_results;
        vision.generate_image;
        vision.register_image;
        attachments.read_attachment;
        session.read_session_history;
        session.read_asset;
        session.snapshot;
        // Control-plane methods resolve to the existing permission-checked handlers.
        host.contracts => read_tool_contracts;
        host.find_methods => find_library_methods;
        host.guide => read_program_guide;
        host.list_skills;
        host.read_skill;
        host.ask_user;
    }
};

pub(crate) struct ToolRegistry;

impl ToolRegistry {
    pub const fn all() -> &'static [ToolSpec] {
        SPECS
    }

    pub fn find(name: &str) -> Option<&'static ToolSpec> {
        Self::all().iter().find(|spec| spec.name == name)
    }

    pub fn qualified_hint(bare: &str) -> Option<String> {
        let mut matches = Self::all()
            .iter()
            .filter(|spec| !spec.library.is_empty() && (spec.name == bare || spec.method == bare))
            .map(|spec| format!("{}.{}", spec.library, spec.method))
            .collect::<Vec<_>>();
        matches.sort();
        matches.dedup();
        (matches.len() == 1).then(|| matches.pop().unwrap())
    }

    pub fn builtin_libraries() -> Libraries {
        let mut libraries = Libraries::new();
        for spec in Self::all() {
            libraries
                .entry(spec.library.to_owned())
                .or_default()
                .insert(spec.method.to_owned(), spec.name.to_owned());
        }
        libraries
    }

    pub fn allows_in_mode(name: &str, mode: PermissionMode) -> Option<bool> {
        Self::find(name).map(|spec| spec.allow.allows(mode))
    }

    pub fn is_in_local_fallback(name: &str) -> bool {
        Self::find(name).is_some_and(|spec| spec.fallback_visible)
    }

    pub fn is_tracked_mutation(name: &str) -> bool {
        Self::find(name).is_some_and(|spec| spec.tracks_mutation)
    }

    pub fn records_written_path(name: &str) -> bool {
        Self::find(name).is_some_and(|spec| spec.records_written_path)
    }

    pub fn write_summary_label(name: &str) -> Option<&'static str> {
        Self::find(name).and_then(|spec| spec.write_summary_label)
    }

    pub const fn catalog_len(kind: CatalogKind) -> usize {
        let mut len = 0;
        let mut i = 0;
        while i < SPECS.len() {
            if SPECS[i].catalog as u8 == kind as u8 {
                len += 1;
            }
            i += 1;
        }
        len
    }

    pub const fn catalog_names<const N: usize>(kind: CatalogKind) -> [&'static str; N] {
        assert!(N == Self::catalog_len(kind));
        let mut names = [""; N];
        let mut i = 0;
        let mut j = 0;
        while i < SPECS.len() {
            if SPECS[i].catalog as u8 == kind as u8 {
                names[j] = SPECS[i].name;
                j += 1;
            }
            i += 1;
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn tool_names_and_js_methods_are_unique() {
        let mut names = BTreeSet::new();
        let mut methods = BTreeSet::new();
        for spec in ToolRegistry::all() {
            assert!(names.insert(spec.name), "duplicate tool {}", spec.name);
            assert!(
                methods.insert((spec.library, spec.method)),
                "duplicate method {}.{}",
                spec.library,
                spec.method,
            );
        }
    }

    #[test]
    fn read_only_rejects_every_tracked_mutation() {
        for spec in ToolRegistry::all()
            .iter()
            .filter(|spec| spec.tracks_mutation)
        {
            assert!(!PermissionMode::ReadOnly.allows(spec.name), "{}", spec.name);
        }
    }

    #[test]
    fn only_single_path_writes_report_written_paths_and_labels() {
        let writes = [
            ("write_file", "已写入"),
            ("edit_file", "已写入"),
            ("replace_range", "已写入"),
            ("delete_path", "已删除"),
            ("rename_path", "已移动"),
            ("create_dir", "已创建目录"),
        ];
        for spec in ToolRegistry::all() {
            let label = writes
                .iter()
                .find_map(|&(name, label)| (name == spec.name).then_some(label));
            assert_eq!(
                ToolRegistry::write_summary_label(spec.name),
                label,
                "{}",
                spec.name
            );
            assert_eq!(
                ToolRegistry::records_written_path(spec.name),
                label.is_some(),
                "{}",
                spec.name
            );
        }
        for (name, _) in writes {
            assert!(ToolRegistry::is_tracked_mutation(name), "{name}");
        }
    }

    #[test]
    fn qualified_hint_maps_bare_tool_names_to_library_namespaces() {
        assert_eq!(
            ToolRegistry::qualified_hint("read_asset").as_deref(),
            Some("session.read_asset")
        );
        assert_eq!(
            ToolRegistry::qualified_hint("read_session_history").as_deref(),
            Some("session.read_session_history")
        );
        assert_eq!(
            ToolRegistry::qualified_hint("apply_edits").as_deref(),
            Some("io.apply_edits")
        );
        assert_eq!(ToolRegistry::qualified_hint("nonexistent_fn"), None);
    }
}
