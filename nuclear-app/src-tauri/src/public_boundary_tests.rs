use crate::services::queue::display_output_directory;

#[test]
fn output_directory_hides_windows_verbatim_prefixes() {
    assert_eq!(
        display_output_directory(r"\\?\C:\Users\Example\Downloads".into()),
        r"C:\Users\Example\Downloads"
    );
    assert_eq!(
        display_output_directory(r"\\?\UNC\server\share\Downloads".into()),
        r"\\server\share\Downloads"
    );
    assert_eq!(
        display_output_directory(r"\\?\Volume{1234}\Downloads".into()),
        r"\\?\Volume{1234}\Downloads"
    );
}

#[test]
fn public_command_registry_is_exact_and_ordered() {
    let source = include_str!("lib.rs");
    let handler = source
        .split(".invoke_handler(tauri::generate_handler![")
        .nth(1)
        .and_then(|tail| tail.split("])").next())
        .expect("invoke handler command list");
    let commands = handler
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.strip_suffix(',').expect("comma-terminated command"))
        .collect::<Vec<_>>();

    assert_eq!(
        commands,
        [
            "begin_inspection",
            "cancel_operation",
            "dismiss_operation",
            "cancel_all_downloads",
            "get_app_snapshot",
            "add_inspection_result_to_queue",
            "update_queue_item",
            "remove_queue_items",
            "enqueue_queue_items",
            "check_downloader_runtime",
            "check_runtime_update",
            "begin_runtime_update",
            "default_download_dir",
            "validate_output_directory",
            "check_app_update",
            "begin_app_update",
            "export_diagnostics",
            "clear_diagnostics",
        ]
    );
}

#[test]
fn persistent_state_opens_only_after_single_instance_registration() {
    let production = include_str!("lib.rs");
    let plugin = production
        .find(".plugin(tauri_plugin_single_instance::init")
        .expect("single-instance plugin registration");
    let setup = production
        .find(".setup(move |app|")
        .expect("application setup hook");
    let persistent_state = production
        .find("StateStore::open_default()")
        .expect("persistent state initialization");

    assert!(plugin < setup);
    assert!(setup < persistent_state);
}

#[test]
fn download_worker_pool_is_started_once_during_setup() {
    let production = include_str!("lib.rs");
    assert_eq!(production.matches("spawn_download_workers(").count(), 1);
    assert!(include_str!("services/downloads.rs").contains("for _ in 0..5"));
    let enqueue = include_str!("services/queue.rs")
        .split("fn enqueue_queue_items(")
        .nth(1)
        .and_then(|tail| tail.split("\n}\n").next())
        .expect("enqueue command body");
    assert!(!enqueue.contains("spawn_download_workers"));
}
